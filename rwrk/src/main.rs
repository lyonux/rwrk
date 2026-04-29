use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use chrono::FixedOffset;
use clap::Parser;
use futures::StreamExt;
use prometheus::{HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Registry, TextEncoder};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "rwrk")]
struct Cli {
    /// Enrollment SSE endpoint URL
    #[arg(short = 'c')]
    config: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registration {
    id: String,
    #[serde(flatten)]
    fields: HashMap<String, String>,
}

struct Metrics {
    requests_total: IntCounter,
    status_codes: IntCounterVec,
    latency: HistogramVec,
    latency_by_status: HistogramVec,
    registry: Registry,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();

        let requests_total = IntCounter::new("rwrk_requests_total", "Total requests sent").unwrap();
        registry.register(Box::new(requests_total.clone())).unwrap();

        let status_codes = IntCounterVec::new(
            prometheus::Opts::new("rwrk_status_codes_total", "HTTP status code counts"),
            &["endpoint", "status"],
        )
        .unwrap();
        registry.register(Box::new(status_codes.clone())).unwrap();

        let latency = HistogramVec::new(
            HistogramOpts::new("rwrk_request_duration_seconds", "Request latency").buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["endpoint"],
        )
        .unwrap();
        registry.register(Box::new(latency.clone())).unwrap();

        let latency_by_status = HistogramVec::new(
            HistogramOpts::new(
                "rwrk_request_duration_by_status_seconds",
                "Request latency by status code (use histogram_quantile for p90/p95/p99)",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["endpoint", "status"],
        )
        .unwrap();
        registry.register(Box::new(latency_by_status.clone())).unwrap();

        Metrics {
            requests_total,
            status_codes,
            latency,
            latency_by_status,
            registry,
        }
    }

    fn record_request(&self, endpoint: &str, status: u16, duration: Duration) {
        self.requests_total.inc();
        self.status_codes
            .with_label_values(&[endpoint, &status.to_string()])
            .inc();
        self.latency
            .with_label_values(&[endpoint])
            .observe(duration.as_secs_f64());
        self.latency_by_status
            .with_label_values(&[endpoint, &status.to_string()])
            .observe(duration.as_secs_f64());
    }

    fn gather(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode_to_string(&metric_families).unwrap()
    }
}

fn parse_expire(expire_str: &str) -> Option<i64> {
    let beijing = chrono::FixedOffset::east_opt(8 * 3600)?;
    let dt = chrono::NaiveDateTime::parse_from_str(expire_str, "%Y.%m.%d-%H:%M")
        .ok()?
        .and_local_timezone(beijing)
        .single()?;
    Some(dt.timestamp())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let enroll_url = cli.config;

    info!("Connecting to enroll at: {}", enroll_url);

    let metrics = Arc::new(Metrics::new());
    let (tx, rx) = watch::channel(None);
    let client = Client::new();

    // Spawn metrics server on port 9090
    let metrics_clone = metrics.clone();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/metrics",
            axum::routing::get(move || {
                let m = metrics_clone.clone();
                async move { axum::response::Html(m.gather()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await.unwrap();
        info!("Metrics server on 0.0.0.0:9090");
        axum::serve(listener, app).await.unwrap();
    });

    // Spawn load test worker
    let worker_client = client.clone();
    let worker_metrics = metrics.clone();
    tokio::spawn(run_worker(rx, worker_client, worker_metrics));

    // SSE connection loop with auto-reconnect
    loop {
        info!("Connecting to enroll SSE: {}", enroll_url);
        match connect_sse(&client, &enroll_url, &tx).await {
            Ok(()) => warn!("SSE connection closed, reconnecting..."),
            Err(e) => error!("SSE error: {}, reconnecting...", e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_sse(
    client: &Client,
    url: &str,
    tx: &watch::Sender<Option<Registration>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = client.get(url).send().await?;
    use eventsource_stream::Eventsource;
    let stream = response.bytes_stream().eventsource();

    tokio::pin!(stream);

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => {
                if !event.data.trim().is_empty() {
                    match serde_json::from_str::<Registration>(&event.data) {
                        Ok(reg) => {
                            info!("Received registration update: {:?}", reg);
                            let _ = tx.send(Some(reg));
                        }
                        Err(e) => error!("Failed to parse registration: {}", e),
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

async fn run_worker(
    mut rx: watch::Receiver<Option<Registration>>,
    client: Client,
    metrics: Arc<Metrics>,
) {
    let mut current_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut last_reg: Option<Registration> = None;

    loop {
        if rx.changed().await.is_err() {
            break;
        }

        let reg = rx.borrow().clone();
        match reg {
            Some(reg) => {
                let connection: u64 = reg
                    .fields
                    .get("connection")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);

                let expire_str = reg.fields.get("expire").cloned().unwrap_or_default();
                let expire = parse_expire(&expire_str);
                let now = chrono::Local::now().timestamp();
                let expired = expire
                    .map(|e| {
                        info!("Now time {}; expire time {}", now, e);
                        e < now
                    })
                    .unwrap_or(true);

                if connection == 0 || expired {
                    if let Some(handle) = current_task.take() {
                        handle.abort();
                        info!("Load test stopped");
                    }
                    if connection == 0 {
                        info!("Connection is 0, idle");
                    }
                    if expired {
                        info!("Expired at {}, stopping", expire_str);
                    }
                    last_reg = None;
                    continue;
                }

                // Restart if connection count or host changed
                let should_restart = match &last_reg {
                    None => true,
                    Some(prev) => {
                        prev.fields.get("connection") != reg.fields.get("connection")
                            || prev.fields.get("host") != reg.fields.get("host")
                    }
                };

                if should_restart {
                    if let Some(handle) = current_task.take() {
                        handle.abort();
                    }

                    let host = reg
                        .fields
                        .get("host")
                        .cloned()
                        .unwrap_or_else(|| "http://127.0.0.1:8080/check".to_string());
                    let rate: usize = reg
                        .fields
                        .get("rate")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(connection as usize);

                    info!(
                        "Starting load test: host={}, connections={}, rate={}, expire={}",
                        host, connection, rate, expire_str
                    );

                    let handle = tokio::spawn(run_load_test(
                        client.clone(),
                        metrics.clone(),
                        host,
                        connection,
                        rate,
                        expire,
                        reg.clone(),
                        rx.clone(),
                    ));
                    current_task = Some(handle);
                }

                last_reg = Some(reg);
            }
            None => {
                if let Some(handle) = current_task.take() {
                    handle.abort();
                    info!("Load test stopped (no registration)");
                }
                last_reg = None;
            }
        }
    }
}

async fn run_load_test(
    client: Client,
    metrics: Arc<Metrics>,
    host: String,
    connections: u64,
    rate: usize,
    expire: Option<i64>,
    _reg: Registration,
    mut rx: watch::Receiver<Option<Registration>>,
) {
    // Use a semaphore for rate limiting
    let semaphore = Arc::new(tokio::sync::Semaphore::new(rate));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Expire timer: auto-stop when expire is reached
    let mut rx_expire = rx.clone();
    let expire_monitor = tokio::spawn(async move {
        let Some(expire_ts) = expire else { return };
        loop {
            let now = chrono::Local::now().timestamp();
            if now >= expire_ts {
                info!("Expired, stopping load test");
                let _ = stop_tx.send(true);
                return;
            }
            let delay = Duration::from_secs((expire_ts - now).max(1) as u64);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = rx_expire.changed() => {
                    // expire may have been updated
                    if let Some(new_reg) = rx_expire.borrow().as_ref() {
                        if let Some(new_expire) = new_reg.fields.get("expire").and_then(|v| parse_expire(v)) {
                            if chrono::Local::now().timestamp() >= new_expire {
                                info!("Updated expire already passed, stopping");
                                let _ = stop_tx.send(true);
                                return;
                            }
                        }
                    }
                }
            }
        }
    });

    // Monitor for rate changes
    let mut rx2 = rx.clone();
    let sem_clone = semaphore.clone();
    let rate_monitor = tokio::spawn(async move {
        loop {
            if rx2.changed().await.is_err() {
                break;
            }
            if let Some(new_reg) = rx2.borrow().clone() {
                if let Some(new_rate) = new_reg
                    .fields
                    .get("rate")
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    let current = sem_clone.available_permits();
                    if new_rate > current {
                        sem_clone.add_permits(new_rate - current);
                    }
                }
            }
        }
    });

    for _ in 0..connections {
        let client = client.clone();
        let metrics = metrics.clone();
        let host = host.clone();
        let sem = semaphore.clone();
        let mut stop_rx = stop_rx.clone();

        let handle = tokio::spawn(async move {
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                tokio::select! {
                    permit = sem.clone().acquire_owned() => {
                        let _permit = permit.unwrap();
                        if *stop_rx.borrow() {
                            break;
                        }
                        let start = std::time::Instant::now();
                        match client.get(&host).send().await {
                            Ok(resp) => {
                                let duration = start.elapsed();
                                let status = resp.status().as_u16();
                                metrics.record_request(&host, status, duration);
                            }
                            Err(_e) => {
                                let duration = start.elapsed();
                                metrics.record_request(&host, 0, duration);
                            }
                        }
                    }
                    _ = stop_rx.changed() => {
                        if *stop_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        handles.push(handle);
    }

    futures::future::join_all(handles).await;
    expire_monitor.abort();
    rate_monitor.abort();
}

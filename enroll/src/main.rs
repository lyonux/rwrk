use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::{
    Router,
    extract::{Query, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use clap::Parser;
use dashmap::DashMap;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use tokio::sync::broadcast;
use tracing::info;

static CONNECTION_ID: AtomicU64 = AtomicU64::new(0);

type RegistrationMap = Arc<DashMap<String, Registration>>;
type NotifierMap = Arc<DashMap<String, broadcast::Sender<()>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub id: String,
    #[serde(flatten)]
    pub fields: HashMap<String, String>,
}

#[derive(Clone)]
struct AppState {
    registrations: RegistrationMap,
    notifiers: NotifierMap,
    listener_counts: Arc<DashMap<String, Arc<()>>>,
}

#[derive(Deserialize)]
struct SetParams {
    id: String,
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

#[derive(Deserialize)]
struct GetParams {
    id: String,
}

#[derive(Parser)]
#[command(name = "enroll")]
struct Cli {
    /// Listen port
    #[arg(short = 'l', default_value = "8081")]
    port: u16,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let addr = format!("0.0.0.0:{}", cli.port);

    let state = AppState {
        registrations: Arc::new(DashMap::new()),
        notifiers: Arc::new(DashMap::new()),
        listener_counts: Arc::new(DashMap::new()),
    };

    // Initialize default registration for "rwrk"
    let mut default_fields = HashMap::new();
    default_fields.insert("connection".to_string(), "0".to_string());
    default_fields.insert("expire".to_string(), "2000.1.1-00:00".to_string());
    state.registrations.insert(
        "rwrk".to_string(),
        Registration {
            id: "rwrk".to_string(),
            fields: default_fields,
        },
    );

    let app = Router::new()
        .route("/set", get(set_handler).post(set_handler))
        .route("/get", get(get_handler))
        .route("/info", get(info_handler))
        .with_state(state);

    // Create listener with TCP keepalive so accepted connections inherit it
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    socket.set_reuse_address(true).unwrap();
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(5))
        .with_interval(Duration::from_secs(5));
    socket.set_tcp_keepalive(&ka).unwrap();
    let bind_addr: std::net::SocketAddr = addr.parse().unwrap();
    socket.bind(&bind_addr.into()).unwrap();
    socket.listen(128).unwrap();

    let listener = tokio::net::TcpListener::from_std(socket.into()).unwrap();
    info!("enroll server listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

async fn set_handler(
    State(state): State<AppState>,
    Query(params): Query<SetParams>,
) -> axum::Json<serde_json::Value> {
    let id = params.id.clone();
    let registration = Registration {
        id: params.id,
        fields: params.extra,
    };

    info!("Registering: {:?}", registration);

    let json_val = serde_json::to_value(&registration).unwrap();
    state.registrations.insert(id.clone(), registration);

    // Ensure notifier exists and notify all SSE listeners
    let tx = state
        .notifiers
        .entry(id)
        .or_insert_with(|| broadcast::channel(16).0)
        .clone();
    let _ = tx.send(());

    axum::Json(json_val)
}

async fn get_handler(
    State(state): State<AppState>,
    Query(params): Query<GetParams>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let id = params.id;

    // Ensure a notifier exists for this id
    let tx = state
        .notifiers
        .entry(id.clone())
        .or_insert_with(|| broadcast::channel(16).0)
        .clone();

    // Track listener count — unique per connection via atomic counter
    let conn_id = CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
    let listener_key = format!("{}:{}", id, conn_id);
    state
        .listener_counts
        .insert(listener_key.clone(), Arc::new(()));

    // Get initial data
    let initial_data = state
        .registrations
        .get(&id)
        .map(|r| serde_json::to_value(r.value()).unwrap())
        .unwrap_or_else(|| json!({"id": id}));

    let registrations = state.registrations.clone();
    let listener_counts = state.listener_counts.clone();

    let stream = async_stream::stream! {
        // Guard lives inside the stream so it drops only when the stream is dropped
        struct ListenerGuard {
            key: String,
            counts: Arc<DashMap<String, Arc<()>>>,
        }
        impl Drop for ListenerGuard {
            fn drop(&mut self) {
                self.counts.remove(&self.key);
            }
        }
        let _guard = ListenerGuard {
            key: listener_key,
            counts: listener_counts,
        };

        // Send initial data
        let event = Event::default().data(serde_json::to_string(&initial_data).unwrap());
        yield Ok::<_, Infallible>(event);

        let mut rx = tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(_) => {
                    if let Some(reg) = registrations.get(&id) {
                        let data = serde_json::to_value(reg.value()).unwrap();
                        let event = Event::default().data(serde_json::to_string(&data).unwrap());
                        yield Ok(event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(5)))
}

async fn info_handler(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    let registrations: Vec<serde_json::Value> = state
        .registrations
        .iter()
        .map(|r| serde_json::to_value(r.value()).unwrap())
        .collect();

    // Count listeners per id
    let mut listener_info: HashMap<String, usize> = HashMap::new();
    for entry in state.registrations.iter() {
        let id = entry.key().clone();
        let count = state
            .listener_counts
            .iter()
            .filter(|e| e.key().starts_with(&format!("{}:", &id)))
            .count();
        listener_info.insert(id, count);
    }

    let listeners_list: Vec<serde_json::Value> = listener_info
        .iter()
        .map(|(id, count)| {
            json!({
                "id": id,
                "listeners": count,
            })
        })
        .collect();

    axum::Json(json!({
        "registrations": registrations,
        "listeners": listeners_list,
    }))
}

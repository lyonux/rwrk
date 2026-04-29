use axum::{
    Router,
    routing::{get, post},
};
use clap::Parser;

use echo;
use echo::handler;

#[derive(Parser)]
#[command(name = "pon")]
struct Cli {
    /// Listen port
    #[arg(short = 'l', default_value = "8080")]
    port: u16,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let addr = format!("0.0.0.0:{}", cli.port);

    let app = Router::new()
        .route("/", get(echo::handler::check))
        .route("/check", get(echo::handler::check))
        .route("/ping", get(echo::handler::ping))
        .route("/post", post(echo::handler::post_data))
        .route("/ws", get(handler::ws_echo));

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("pon server listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

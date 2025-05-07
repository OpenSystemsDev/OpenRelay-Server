mod relay;
mod types;

use relay::RelayServer;
use std::env;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() {
    // Initialize logging with dynamic log level
    let log_level = match env::var("LOG_LEVEL")
        .unwrap_or_else(|_| "info".to_string())
        .as_str()
    {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // Get port from system environment variables or use default
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .expect("PORT must be a number");

    // Get bind address from environment or use default
    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());

    let addr: std::net::SocketAddr = format!("{}:{}", bind_addr, port)
        .parse()
        .expect("Invalid bind address or port");

    info!("Starting OpenRelay WebSocket server on {}", addr);

    // Print versioning information
    info!("OpenRelay Server v{}", env!("CARGO_PKG_VERSION"));

    // Create and run the relay server
    let server = RelayServer::new(addr);
    server.run().await;
}

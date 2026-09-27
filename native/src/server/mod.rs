mod handler;
mod runtime;
mod upstream;

use std::{
    env,
    io::{self, Write},
    path::Path,
};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, Method, header},
    routing::{get, post},
};
use serde_json::json;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{Any, CorsLayer};

use crate::{config::AdapterConfig, storage::Storage};

use handler::{health, messages};
use runtime::{Arguments, bind_available, shutdown_signal};
use upstream::build_client;

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 128;

#[derive(Clone)]
struct AppState {
    config: AdapterConfig,
    client: reqwest::Client,
    storage: Storage,
}

pub(super) async fn run() -> Result<(), String> {
    let arguments = Arguments::parse()?;
    let config = AdapterConfig::load(&arguments.config).await?;
    let client = build_client(&config)?;
    let base_dir = arguments
        .config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let (storage, writer_handle) = Storage::start(base_dir);
    let state = AppState {
        config,
        client,
        storage,
    };
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("anthropic-version"),
            HeaderName::from_static("x-api-key"),
        ]);
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/messages", post(messages))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(ConcurrencyLimitLayer::new(MAX_IN_FLIGHT))
        .layer(cors)
        .with_state(state);

    let (listener, port) = bind_available(arguments.port).await?;
    let ready = json!({"url": format!("http://localhost:{port}"), "port": port});
    println!("CLAUDE_ADAPTER_READY={ready}");
    io::stdout().flush().map_err(|error| error.to_string())?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(
            env::var_os("CLAUDE_ADAPTER_STDIN_SHUTDOWN").is_some(),
        ))
        .await
        .map_err(|error| format!("Server failed: {error}"))?;
    writer_handle
        .await
        .map_err(|error| format!("JSONL writer failed to shut down: {error}"))?;
    Ok(())
}

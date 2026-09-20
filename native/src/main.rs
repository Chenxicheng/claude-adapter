mod config;
mod converter;
mod error;
mod storage;
mod stream;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use config::AdapterConfig;
use converter::{convert_request, convert_response, validate_request};
use error::AppError;
use serde_json::{Value, json};
use storage::Storage;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{Any, CorsLayer};

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 128;
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct AppState {
    config: AdapterConfig,
    client: reqwest::Client,
    storage: Storage,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("[adapter] {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
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

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "adapter": "claude-adapter",
        "dropped_jsonl_records": state.storage.dropped_count(),
    }))
}

async fn messages(State(state): State<AppState>, body: Result<Bytes, BytesRejection>) -> Response {
    let request_id = request_id();
    let body = match body {
        Ok(body) => body,
        Err(_) => {
            return with_request_id(
                AppError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Request body exceeds the 32 MiB limit",
                )
                .into_response(),
                &request_id,
            );
        }
    };
    let anthropic: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return with_request_id(
                AppError::bad_request(format!("body: Invalid JSON: {error}")).into_response(),
                &request_id,
            );
        }
    };
    if let Err(error) = validate_request(&anthropic) {
        return with_request_id(error.into_response(), &request_id);
    }
    let model = anthropic["model"].as_str().expect("validated model");
    let streaming = anthropic.get("stream").and_then(Value::as_bool) == Some(true);
    let response = handle_messages(&state, &request_id, &anthropic).await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            record_error(&state, &request_id, model, streaming, &error);
            error.into_response()
        }
    };
    with_request_id(response, &request_id)
}

fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    response
}

async fn handle_messages(
    state: &AppState,
    request_id: &str,
    anthropic: &Value,
) -> Result<Response, AppError> {
    let model = anthropic["model"].as_str().expect("validated model");
    let streaming = anthropic.get("stream").and_then(Value::as_bool) == Some(true);
    eprintln!("-> {model} [sent] {request_id}");
    let openai = convert_request(anthropic, &state.config.base_url)?;
    let response = state
        .client
        .post(state.config.chat_completions_url())
        .json(&openai)
        .send()
        .await
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;
    if !response.status().is_success() {
        return Err(upstream_error(response, &openai).await);
    }

    if streaming {
        let body = stream::transform_stream(
            response,
            model.to_owned(),
            state.config.base_url.clone(),
            state.storage.clone(),
            request_id.to_owned(),
        );
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .header("x-accel-buffering", "no")
            .body(body)
            .expect("valid streaming response");
        eprintln!("<- {model} [streaming] {request_id}");
        return Ok(response);
    }

    let openai_response: Value = response
        .json()
        .await
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;
    let converted = convert_response(&openai_response, model)?;
    if openai_response
        .get("usage")
        .is_some_and(is_non_empty_object)
    {
        let mut record = json!({
            "timestamp": now(),
            "schemaVersion": 2,
            "provider": state.config.base_url,
            "modelName": model,
            "streaming": false,
            "usageStatus": "complete",
            "usage": openai_response["usage"],
        });
        if let Some(actual_model) = openai_response.get("model") {
            record["model"] = actual_model.clone();
        }
        state.storage.record_usage(record);
    }
    eprintln!("<- {model} [received] {request_id}");
    Ok(Json(converted).into_response())
}

async fn upstream_error(response: reqwest::Response, request: &Value) -> AppError {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let text = response
        .text()
        .await
        .unwrap_or_else(|error| format!("Failed to read upstream error: {error}"));
    build_upstream_error(status, text, upstream_request_shape(request))
}

fn build_upstream_error(status: StatusCode, text: String, request_shape: Value) -> AppError {
    let details =
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text.clone()));
    let message = details
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or(&text)
        .to_owned();
    AppError::new(status, message).with_details(json!({
        "response": details,
        "upstreamRequestShape": request_shape,
    }))
}

fn upstream_request_shape(request: &Value) -> Value {
    let object = request.as_object();
    let mut top_level_fields = object
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    top_level_fields.sort();

    let mut message_roles = Vec::new();
    let mut content_types = BTreeMap::<String, u64>::new();
    if let Some(messages) = request.get("messages").and_then(Value::as_array) {
        for message in messages {
            message_roles.push(
                message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid")
                    .to_owned(),
            );
            match message.get("content") {
                Some(Value::String(_)) => *content_types.entry("string".into()).or_default() += 1,
                Some(Value::Array(parts)) => {
                    for part in parts {
                        let kind = part
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("invalid");
                        *content_types.entry(kind.to_owned()).or_default() += 1;
                    }
                }
                Some(Value::Null) | None => *content_types.entry("null".into()).or_default() += 1,
                Some(_) => *content_types.entry("invalid".into()).or_default() += 1,
            }
        }
    }

    let tools = request.get("tools").and_then(Value::as_array);
    let tool_function_fields = tools
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("function").and_then(Value::as_object))
        .flat_map(|function| function.keys().cloned())
        .collect::<BTreeSet<_>>();
    let tool_choice_type = request.get("tool_choice").and_then(|choice| match choice {
        Value::String(kind) => Some(kind.as_str()),
        Value::Object(choice) => choice.get("type").and_then(Value::as_str),
        _ => None,
    });
    let model_family = request
        .get("model")
        .and_then(Value::as_str)
        .map(|model| model.to_ascii_lowercase())
        .map(|model| {
            if model.starts_with("glm-5") {
                "glm-5"
            } else if model.starts_with("qwen3") {
                "qwen3"
            } else if model.starts_with("gpt-5")
                || model
                    .strip_prefix('o')
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(|character| character.is_ascii_digit())
            {
                "openai-reasoning"
            } else {
                "generic"
            }
        })
        .unwrap_or("unknown");

    json!({
        "topLevelFields": top_level_fields,
        "messageRoles": message_roles,
        "messageContentTypes": content_types,
        "maxTokenField": if request.get("max_completion_tokens").is_some() {
            Some("max_completion_tokens")
        } else if request.get("max_tokens").is_some() {
            Some("max_tokens")
        } else {
            None
        },
        "toolCount": tools.map_or(0, Vec::len),
        "toolFunctionFields": tool_function_fields,
        "toolChoiceType": tool_choice_type,
        "modelFamily": model_family,
        "stream": request.get("stream").and_then(Value::as_bool),
    })
}

fn record_error(
    state: &AppState,
    request_id: &str,
    model: &str,
    streaming: bool,
    error: &AppError,
) {
    state.storage.record_error(error_record_value(
        request_id,
        &state.config.base_url,
        model,
        streaming,
        error,
    ));
}

fn error_record_value(
    request_id: &str,
    provider: &str,
    model: &str,
    streaming: bool,
    error: &AppError,
) -> Value {
    let mut details = json!({
        "message": error.message,
        "status": error.status.as_u16(),
    });
    let mut request_shape = None;
    if let Some(response) = &error.details {
        if let Some(shape) = response.get("upstreamRequestShape") {
            request_shape = Some(shape.clone());
            details["response"] = response["response"].clone();
        } else {
            details["response"] = response.clone();
        }
    }
    let mut record = json!({
        "timestamp": now(),
        "requestId": request_id,
        "provider": provider,
        "modelName": model,
        "streaming": streaming,
        "error": details,
    });
    if let Some(shape) = request_shape {
        record["upstreamRequestShape"] = shape;
    }
    record
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn is_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
}

fn request_id() -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("req_{}_{}", Utc::now().timestamp_millis(), counter)
}

fn build_client(config: &AdapterConfig) -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    if let Some(custom) = &config.upstream_headers {
        for (name, value) in custom {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("Invalid upstream header {name}: {error}"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| format!("Invalid upstream header value: {error}"))?;
            headers.insert(name, value);
        }
    }
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let authorization = HeaderValue::from_str(&format!("Bearer {}", config.api_key))
        .map_err(|error| format!("Invalid API key header: {error}"))?;
    headers.insert(header::AUTHORIZATION, authorization);
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("claude-adapter/2.0.1")
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("Failed to create upstream client: {error}"))
}

async fn bind_available(preferred: u16) -> Result<(TcpListener, u16), String> {
    for port in preferred..=u16::MAX {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                let actual_port = listener
                    .local_addr()
                    .map_err(|error| format!("Failed to inspect listener: {error}"))?
                    .port();
                return Ok((listener, actual_port));
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(error) => return Err(format!("Failed to bind port {port}: {error}")),
        }
    }
    Err(format!("No available port at or above {preferred}"))
}

async fn shutdown_signal(stdin_enabled: bool) {
    let stdin = async move {
        if stdin_enabled {
            let _ = tokio::io::stdin().read_u8().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
            _ = stdin => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = stdin => {},
        }
    }
}

struct Arguments {
    config: PathBuf,
    port: u16,
}

impl Arguments {
    fn parse() -> Result<Self, String> {
        let mut values = env::args().skip(1);
        let mut config = None;
        let mut port = 3080;
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--config" => config = values.next().map(PathBuf::from),
                "--port" => {
                    let value = values.next().ok_or("--port requires a value")?;
                    port = value
                        .parse()
                        .map_err(|_| format!("Invalid port: {value}"))?;
                }
                "--version" | "-V" => {
                    println!("claude-adapter-native 2.0.1");
                    std::process::exit(0);
                }
                other => return Err(format!("Unknown argument: {other}")),
            }
        }
        Ok(Self {
            config: config.ok_or("--config is required")?,
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_upstream_error_keeps_status_and_sanitized_request_shape() {
        let request = json!({
            "model": "glm-5.2",
            "messages": [
                {"role": "system", "content": "secret system prompt"},
                {"role": "user", "content": [
                    {"type": "text", "text": "secret prompt"},
                    {"type": "image_url", "image_url": {"url": "https://secret.test/a.png"}}
                ]}
            ],
            "max_tokens": 128,
            "stream": true,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup_secret",
                    "description": "secret description",
                    "parameters": {"type": "object", "secret_schema": true}
                }
            }],
            "tool_choice": "auto",
            "thinking": {"type": "enabled"},
            "tool_stream": true
        });
        let shape = upstream_request_shape(&request);
        let serialized = shape.to_string();
        for secret in [
            "secret system prompt",
            "secret prompt",
            "https://secret.test/a.png",
            "lookup_secret",
            "secret description",
            "secret_schema",
        ] {
            assert!(!serialized.contains(secret), "shape leaked {secret}");
        }
        assert_eq!(shape["modelFamily"], "glm-5");
        assert_eq!(shape["toolCount"], 1);
        assert_eq!(shape["toolChoiceType"], "auto");
        assert_eq!(shape["messageContentTypes"]["image_url"], 1);
        assert_eq!(
            shape["toolFunctionFields"],
            json!(["description", "name", "parameters"])
        );

        let error = build_upstream_error(
            StatusCode::NOT_ACCEPTABLE,
            "Not Acceptable".to_owned(),
            shape.clone(),
        );
        assert_eq!(error.status, StatusCode::NOT_ACCEPTABLE);
        assert_eq!(error.message, "Not Acceptable");
        assert_eq!(
            error.details.as_ref().unwrap()["response"],
            "Not Acceptable"
        );
        assert_eq!(
            error.details.as_ref().unwrap()["upstreamRequestShape"],
            shape
        );
        let record =
            error_record_value("req_1", "https://provider.test/v1", "glm-5.2", true, &error);
        assert_eq!(record["error"]["status"], 406);
        assert_eq!(record["error"]["response"], "Not Acceptable");
        assert_eq!(record["upstreamRequestShape"], shape);
    }
}

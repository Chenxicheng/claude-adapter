use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    Json,
    body::Bytes,
    extract::{State, rejection::BytesRejection},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    protocol::{convert_request_with_mode, convert_response, transform_stream, validate_request},
};

use super::{
    AppState,
    upstream::{STREAM_RESPONSE_HEADERS_TIMEOUT, await_upstream_headers, upstream_error},
};

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "adapter": "claude-adapter",
        "dropped_jsonl_records": state.storage.dropped_count(),
    }))
}

pub(super) async fn messages(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
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
    let openai = convert_request_with_mode(
        anthropic,
        &state.config.base_url,
        state.config.upstream_capabilities.assistant_prefill,
    )?;
    eprintln!("-> {model} [sent] {request_id}");
    let request = state
        .client
        .post(state.config.chat_completions_url())
        .json(&openai);
    let response = if streaming {
        await_upstream_headers(request.send(), STREAM_RESPONSE_HEADERS_TIMEOUT).await?
    } else {
        request
            .send()
            .await
            .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?
    };
    let headers_received_at = tokio::time::Instant::now();
    if !response.status().is_success() {
        return Err(upstream_error(response, &openai).await);
    }

    if streaming {
        let body = transform_stream(
            response,
            model.to_owned(),
            state.config.base_url.clone(),
            state.storage.clone(),
            request_id.to_owned(),
            headers_received_at,
            openai
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tool| {
                    tool.pointer("/function/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect(),
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
    let has_usage = openai_response
        .get("usage")
        .is_some_and(is_non_empty_object);
    let empty_end_turn = converted["stop_reason"] == "end_turn"
        && converted["content"].as_array().is_some_and(Vec::is_empty);
    if has_usage || empty_end_turn {
        let mut record = json!({
            "timestamp": now(),
            "schemaVersion": 2,
            "provider": state.config.base_url,
            "modelName": model,
            "streaming": false,
            "usageStatus": if has_usage { "complete" } else { "missing" },
            "outcome": if empty_end_turn { "empty_end_turn" } else { "usable" },
        });
        if has_usage {
            record["usage"] = openai_response["usage"].clone();
        }
        if let Some(actual_model) = openai_response.get("model") {
            record["model"] = actual_model.clone();
        }
        state.storage.record_usage(record);
    }
    eprintln!("<- {model} [received] {request_id}");
    Ok(Json(converted).into_response())
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

#[cfg(test)]
#[path = "../../tests/unit/server/handler.rs"]
mod tests;

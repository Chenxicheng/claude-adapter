use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use serde_json::{Value, json};

use crate::{config::AdapterConfig, error::AppError};

pub(super) const STREAM_RESPONSE_HEADERS_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) async fn await_upstream_headers<F>(
    response: F,
    timeout: Duration,
) -> Result<reqwest::Response, AppError>
where
    F: Future<Output = reqwest::Result<reqwest::Response>>,
{
    tokio::time::timeout(timeout, response)
        .await
        .map_err(|_| {
            AppError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "Upstream timed out waiting for response headers",
            )
        })?
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))
}

pub(super) async fn upstream_error(response: reqwest::Response, request: &Value) -> AppError {
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
            let name = model.rsplit('/').next().unwrap_or(&model);
            if name.starts_with("glm-5") {
                "glm-5"
            } else if name.starts_with("qwen3") {
                "qwen3"
            } else if name.starts_with("gpt-5")
                || name
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
    let terminal_assistant_prefill = request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.last())
        .is_some_and(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && match message.get("content") {
                    Some(Value::String(content)) => !content.is_empty(),
                    Some(Value::Array(content)) => !content.is_empty(),
                    _ => false,
                }
        });

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
        "maxTokenValue": request
            .get("max_completion_tokens")
            .or_else(|| request.get("max_tokens"))
            .and_then(Value::as_u64),
        "messageCount": request
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::len),
        "toolCount": tools.map_or(0, Vec::len),
        "toolFunctionFields": tool_function_fields,
        "toolChoiceType": tool_choice_type,
        "modelFamily": model_family,
        "stream": request.get("stream").and_then(Value::as_bool),
        "thinkingPresent": request.get("thinking").is_some()
            || request.get("enable_thinking").is_some()
            || request.get("reasoning_effort").is_some(),
        "terminalAssistantPrefill": terminal_assistant_prefill,
    })
}

pub(super) fn build_client(config: &AdapterConfig) -> Result<reqwest::Client, String> {
    let headers = build_default_headers(config)?;
    reqwest::Client::builder()
        .default_headers(headers)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("Failed to create upstream client: {error}"))
}

fn build_default_headers(config: &AdapterConfig) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    if let Some(custom) = &config.upstream_headers {
        for (name, value) in custom {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("Invalid upstream header {name}: {error}"))?;
            if [
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::ACCEPT,
                header::CONTENT_LENGTH,
                header::HOST,
            ]
            .contains(&header_name)
            {
                return Err(format!(
                    "Upstream header {name} is reserved and cannot be configured"
                ));
            }
            let value = HeaderValue::from_str(value)
                .map_err(|error| format!("Invalid upstream header value: {error}"))?;
            headers.insert(header_name, value);
        }
    }
    if !headers.contains_key(header::USER_AGENT) {
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static("OpenAI/JS 4.76.0"),
        );
    }
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let api_key = config.resolve_api_key()?;
    let authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|error| format!("Invalid API key header: {error}"))?;
    headers.insert(header::AUTHORIZATION, authorization);
    Ok(headers)
}

#[cfg(test)]
#[path = "../../tests/unit/server/upstream.rs"]
mod tests;

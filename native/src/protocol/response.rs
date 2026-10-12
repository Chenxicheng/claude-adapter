use axum::http::StatusCode;
use rand::Rng;
use serde_json::{Value, json};

use crate::error::AppError;

use super::request::{IdContext, unique_tool_id};

pub fn convert_response(response: &Value, original_model: &str) -> Result<Value, AppError> {
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| upstream_protocol_error("response is missing choices"))?;
    if choices.len() != 1 {
        return Err(upstream_protocol_error(
            "response must contain exactly one choice",
        ));
    }
    let choice = &choices[0];
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| upstream_protocol_error("response is missing choices[0].message"))?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(upstream_protocol_error(
            "response message role must be assistant",
        ));
    }
    for field in ["content", "refusal"] {
        if message
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(upstream_protocol_error(format!(
                "response message {field} must be a string or null"
            )));
        }
    }
    for field in ["reasoning_content", "reasoning"] {
        if message
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(upstream_protocol_error(format!(
                "response message {field} must be a string or null"
            )));
        }
    }
    for field in ["audio", "function_call"] {
        if message.get(field).is_some_and(|value| !value.is_null()) {
            return Err(upstream_protocol_error(format!(
                "response message {field} is unsupported"
            )));
        }
    }
    if message.get("annotations").is_some_and(|value| {
        !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty())
    }) {
        return Err(upstream_protocol_error(
            "response message annotations are unsupported",
        ));
    }
    let mut content = Vec::new();
    let reasoning = reasoning_content(&choice["message"]);
    if !reasoning.is_empty() {
        content.push(json!({"type": "thinking", "thinking": reasoning}));
    }
    if let Some(text) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type": "text", "text": text}));
    }
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    if let Some(text) = refusal {
        content.push(json!({"type": "text", "text": text}));
    }
    let mut ids = IdContext::default();
    if message
        .get("tool_calls")
        .is_some_and(|calls| !calls.is_null() && !calls.is_array())
    {
        return Err(upstream_protocol_error(
            "response message tool_calls must be an array",
        ));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            if call.get("type").and_then(Value::as_str) != Some("function") {
                return Err(upstream_protocol_error(
                    "only function tool calls are supported",
                ));
            }
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    upstream_protocol_error("tool call is missing function.arguments")
                })?;
            let input: Value = serde_json::from_str(if arguments.is_empty() {
                "{}"
            } else {
                arguments
            })
            .map_err(|error| {
                upstream_protocol_error(format!("tool arguments are invalid JSON: {error}"))
            })?;
            if !input.is_object() {
                return Err(upstream_protocol_error(
                    "tool arguments must decode to a JSON object",
                ));
            }
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| upstream_protocol_error("tool call is missing function.name"))?;
            if call
                .get("id")
                .is_some_and(|id| !id.is_null() && !id.is_string())
            {
                return Err(upstream_protocol_error(
                    "tool call id must be a string or null",
                ));
            }
            let id = unique_tool_id(
                call.get("id").and_then(Value::as_str).unwrap_or(""),
                &mut ids,
            );
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .ok_or_else(|| upstream_protocol_error("response is missing finish_reason"))?;
    let mapped_finish = map_finish_reason(finish)?;
    let has_tools = content.iter().any(|block| block["type"] == "tool_use");
    if finish != "length" && (finish == "tool_calls") != has_tools {
        return Err(upstream_protocol_error(
            "response tool calls do not match finish_reason",
        ));
    }
    let filtered = finish == "content_filter";
    let refusal_explanation =
        refusal.unwrap_or("Response withheld by the upstream content filter.");
    if filtered && content.is_empty() {
        content.push(json!({"type": "text", "text": refusal_explanation}));
    }
    let is_refusal = filtered || refusal.is_some();
    let stop_reason = if is_refusal { "refusal" } else { mapped_finish };
    let stop_details = if is_refusal {
        json!({"type": "refusal", "category": Value::Null, "explanation": refusal_explanation})
    } else {
        Value::Null
    };
    let usage = response_usage(response.get("usage"))?;
    let id = match response.get("id") {
        Some(Value::String(id)) if !id.is_empty() => format!("msg_{id}"),
        None | Some(Value::Null) | Some(Value::String(_)) => generated_message_id(),
        Some(_) => return Err(upstream_protocol_error("response id must be a string")),
    };
    let mut result = json!({
        "id": id, "type": "message", "role": "assistant", "content": content,
        "model": original_model, "stop_reason": stop_reason, "stop_sequence": Value::Null,
        "usage": usage
    });
    if is_refusal {
        result["stop_details"] = stop_details;
    }
    Ok(result)
}

pub(super) fn reasoning_content(message: &Value) -> &str {
    ["reasoning_content", "reasoning"]
        .into_iter()
        .filter_map(|field| message.get(field).and_then(Value::as_str))
        .find(|text| !text.is_empty())
        .unwrap_or("")
}

pub(super) fn map_finish_reason(reason: &str) -> Result<&'static str, AppError> {
    match reason {
        "stop" => Ok("end_turn"),
        "length" => Ok("max_tokens"),
        "tool_calls" => Ok("tool_use"),
        "content_filter" => Ok("refusal"),
        "function_call" => Err(upstream_protocol_error(
            "legacy function_call finish reason is unsupported",
        )),
        other => Err(upstream_protocol_error(format!(
            "unknown upstream finish reason: {other}"
        ))),
    }
}

fn response_usage(usage: Option<&Value>) -> Result<Value, AppError> {
    let usage = match usage {
        None | Some(Value::Null) => None,
        Some(Value::Object(usage)) => Some(usage),
        Some(_) => return Err(upstream_protocol_error("response usage must be an object")),
    };
    let token = |field: &str| -> Result<Value, AppError> {
        match usage.and_then(|usage| usage.get(field)) {
            None | Some(Value::Null) => Ok(json!(0)),
            Some(value) => value.as_u64().map(Value::from).ok_or_else(|| {
                upstream_protocol_error(format!("response usage.{field} must be an integer"))
            }),
        }
    };
    Ok(json!({
        "input_tokens": token("prompt_tokens")?,
        "output_tokens": token("completion_tokens")?,
    }))
}

fn generated_message_id() -> String {
    let suffix: String = (0..24)
        .map(|_| {
            const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            CHARS[rand::rng().random_range(0..CHARS.len())] as char
        })
        .collect();
    format!("msg_{suffix}")
}

fn upstream_protocol_error(message: impl Into<String>) -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, message)
}

#[cfg(test)]
#[path = "../../tests/unit/protocol/response.rs"]
mod tests;

use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::Rng;
use serde_json::{Map, Value, json};
use url::Url;

use crate::error::AppError;

const BILLING_HEADER: &str = "x-anthropic-billing-header:";
const IMAGE_MEDIA_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];

pub fn validate_request(body: &Value) -> Result<(), AppError> {
    let object = body
        .as_object()
        .ok_or_else(|| AppError::bad_request("body: Request body must be an object"))?;
    let mut errors = Vec::new();

    if object
        .get("model")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push("model: model is required and must be a string".to_owned());
    }
    match object.get("max_tokens").and_then(Value::as_i64) {
        None => errors.push("max_tokens: max_tokens is required and must be a number".to_owned()),
        Some(0) => errors.push(
            "max_tokens: cache-only requests are unsupported; max_tokens must be positive"
                .to_owned(),
        ),
        Some(value) if value < 0 => {
            errors.push("max_tokens: max_tokens must be a positive number".to_owned())
        }
        _ => {}
    }
    match object.get("messages") {
        None => errors.push("messages: messages is required".to_owned()),
        Some(Value::Array(messages)) if messages.is_empty() => {
            errors.push("messages: messages array cannot be empty".to_owned())
        }
        Some(Value::Array(messages)) => validate_messages(messages, &mut errors),
        Some(_) => errors.push("messages: messages must be an array".to_owned()),
    }

    validate_unit_interval(object, "temperature", &mut errors);
    validate_unit_interval(object, "top_p", &mut errors);
    if object
        .get("stream")
        .is_some_and(|value| !value.is_boolean())
    {
        errors.push("stream: stream must be a boolean".to_owned());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::bad_request(errors.join("; ")))
    }
}

fn validate_messages(messages: &[Value], errors: &mut Vec<String>) {
    for (message_index, message) in messages.iter().enumerate() {
        let Some(message) = message.as_object() else {
            errors.push(format!(
                "messages[{message_index}]: message must be an object"
            ));
            continue;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("user" | "assistant") => {}
            Some(_) => errors.push(format!(
                "messages[{message_index}].role: role must be user or assistant"
            )),
            None => errors.push(format!(
                "messages[{message_index}].role: role is required and must be a string"
            )),
        }
        if let Some(content) = message.get("content") {
            match content {
                Value::String(_) | Value::Null => {}
                Value::Array(blocks) => {
                    for (block_index, block) in blocks.iter().enumerate() {
                        let field = format!("messages[{message_index}].content[{block_index}]");
                        match block.as_object() {
                            None => {
                                errors.push(format!("{field}: content block must be an object"))
                            }
                            Some(block)
                                if block
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .is_none_or(str::is_empty) =>
                            {
                                errors
                                    .push(format!("{field}.type: content block type is required"));
                            }
                            _ => {}
                        }
                    }
                }
                _ => errors.push(format!(
                    "messages[{message_index}].content: content must be a string or array"
                )),
            }
        }
    }
}

fn validate_unit_interval(object: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if let Some(value) = object.get(field) {
        let valid = value
            .as_f64()
            .is_some_and(|number| (0.0..=1.0).contains(&number));
        if !valid {
            errors.push(format!("{field}: {field} must be a number between 0 and 1"));
        }
    }
}

pub fn convert_request(body: &Value, base_url: &str) -> Result<Value, AppError> {
    let request = body.as_object().expect("validated request object");
    let model = request["model"].as_str().expect("validated model");
    let mut messages = Vec::new();

    if let Some(system) = request.get("system") {
        let content = system_content(system);
        let content = strip_billing_header(&content);
        if !content.is_empty() {
            messages.push(json!({"role": "system", "content": content}));
        }
    }

    let mut ids = IdContext::default();
    let mut stripped_thinking = false;
    for (message_index, message) in request["messages"]
        .as_array()
        .expect("validated messages")
        .iter()
        .enumerate()
    {
        messages.extend(convert_message(
            message,
            message_index,
            &mut ids,
            &mut stripped_thinking,
        )?);
    }
    if stripped_thinking {
        eprintln!("[adapter] Stripped unsupported Anthropic thinking history from request");
    }
    if messages.is_empty() {
        return Err(AppError::bad_request(
            "No messages after conversion: all input messages had missing content",
        ));
    }

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_owned()));
    output.insert("messages".into(), Value::Array(messages));
    if let Some(stream) = request.get("stream") {
        output.insert("stream".into(), stream.clone());
    }
    let mut max_tokens = request["max_tokens"]
        .as_u64()
        .expect("validated max_tokens");
    if is_azure(base_url) && max_tokens == 1 {
        max_tokens = 32;
    }
    let max_field = if is_openai_reasoning(model) {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    output.insert(max_field.into(), Value::from(max_tokens));
    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        output.insert("stream_options".into(), json!({"include_usage": true}));
    }
    copy_fields(request, &mut output, &["temperature", "top_p"]);
    if let Some(stop) = request.get("stop_sequences") {
        output.insert("stop".into(), stop.clone());
    }
    if let Some(tools) = request
        .get("tools")
        .and_then(Value::as_array)
        .filter(|tools| !tools.is_empty())
    {
        output.insert("tools".into(), convert_tools(tools)?);
    }
    if let Some(choice) = request.get("tool_choice") {
        output.insert("tool_choice".into(), convert_tool_choice(choice)?);
        if choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
        {
            output.insert("parallel_tool_calls".into(), Value::Bool(false));
        }
    }
    apply_model_options(request, &mut output, model)?;
    Ok(Value::Object(output))
}

fn copy_fields(source: &Map<String, Value>, target: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if let Some(value) = source.get(*field) {
            target.insert((*field).to_owned(), value.clone());
        }
    }
}

fn system_content(system: &Value) -> String {
    match system {
        Value::String(content) => content.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn strip_billing_header(content: &str) -> String {
    if !content.starts_with(BILLING_HEADER) {
        return content.to_owned();
    }
    content
        .split_once('\n')
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default()
}

#[derive(Default)]
struct IdContext {
    seen: HashSet<String>,
    mappings: HashMap<String, Vec<String>>,
    result_index: HashMap<String, usize>,
}

fn convert_message(
    message: &Value,
    message_index: usize,
    ids: &mut IdContext,
    stripped_thinking: &mut bool,
) -> Result<Vec<Value>, AppError> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let Some(content) = message.get("content").filter(|content| !content.is_null()) else {
        return Ok(Vec::new());
    };
    if let Some(content) = content.as_str() {
        return Ok(vec![json!({"role": role, "content": content})]);
    }

    let blocks = content.as_array().expect("validated content array");
    if role == "user" {
        convert_user_blocks(blocks, message_index, ids)
    } else {
        Ok(vec![convert_assistant_blocks(
            blocks,
            message_index,
            ids,
            stripped_thinking,
        )?])
    }
}

fn convert_user_blocks(
    blocks: &[Value],
    message_index: usize,
    ids: &mut IdContext,
) -> Result<Vec<Value>, AppError> {
    let mut tool_messages = Vec::new();
    let mut user_parts = Vec::new();
    let mut tool_image_parts = Vec::new();

    for (block_index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => user_parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or("")
            })),
            Some("image") => user_parts.push(convert_image(block)?),
            Some("tool_result") => {
                let original_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let resolved_id = resolve_tool_result_id(original_id, ids);
                let (text, images) = extract_tool_result(block, message_index, block_index)?;
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let text = if text.is_empty() && !images.is_empty() {
                    "Image result follows in the next user message.".to_owned()
                } else {
                    text
                };
                let text = if is_error {
                    format!("Error: {text}")
                } else {
                    text
                };
                tool_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": resolved_id,
                    "content": text,
                }));
                if !images.is_empty() {
                    tool_image_parts.push(json!({
                        "type": "text",
                        "text": format!(
                            "{}Tool result images for tool_call_id={resolved_id}:",
                            if is_error { "Error: " } else { "" }
                        )
                    }));
                    tool_image_parts.extend(images);
                }
            }
            other => {
                return Err(AppError::bad_request(format!(
                    "messages[{message_index}].content[{block_index}].type: unsupported user content block {}",
                    other.unwrap_or("<missing>")
                )));
            }
        }
    }

    let mut result = tool_messages;
    if !user_parts.is_empty() {
        let content = if user_parts.len() == 1 && user_parts[0]["type"] == "text" {
            user_parts[0]["text"].clone()
        } else {
            Value::Array(user_parts)
        };
        result.push(json!({"role": "user", "content": content}));
    }
    if !tool_image_parts.is_empty() {
        result.push(json!({"role": "user", "content": tool_image_parts}));
    }
    Ok(result)
}

fn extract_tool_result(
    block: &Value,
    message_index: usize,
    block_index: usize,
) -> Result<(String, Vec<Value>), AppError> {
    match block.get("content") {
        Some(Value::String(text)) => Ok((text.clone(), Vec::new())),
        Some(Value::Array(parts)) => {
            let mut text = Vec::new();
            let mut images = Vec::new();
            for (part_index, part) in parts.iter().enumerate() {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => text.push(
                        part.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    Some("image") => images.push(convert_image(part)?),
                    other => {
                        return Err(AppError::bad_request(format!(
                            "messages[{message_index}].content[{block_index}].content[{part_index}].type: unsupported tool_result content block {}",
                            other.unwrap_or("<missing>")
                        )));
                    }
                }
            }
            Ok((text.join("\n"), images))
        }
        None | Some(Value::Null) => Ok((String::new(), Vec::new())),
        Some(_) => Err(AppError::bad_request(format!(
            "messages[{message_index}].content[{block_index}].content: tool_result content must be a string or array"
        ))),
    }
}

fn convert_image(block: &Value) -> Result<Value, AppError> {
    let source = block
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::bad_request("image.source is required and must be an object"))?;
    let source_type = source
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request("image.source.type is required"))?;
    let url = match source_type {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("image.source.media_type is required"))?;
            if !IMAGE_MEDIA_TYPES.contains(&media_type) {
                return Err(AppError::bad_request(format!(
                    "Unsupported image media_type {media_type}; use JPEG, PNG, WebP, or non-animated GIF"
                )));
            }
            let data = source
                .get("data")
                .and_then(Value::as_str)
                .filter(|data| !data.is_empty())
                .ok_or_else(|| AppError::bad_request("image.source.data is required"))?;
            STANDARD
                .decode(data)
                .map_err(|_| AppError::bad_request("image.source.data must be valid Base64"))?;
            format!("data:{media_type};base64,{data}")
        }
        "url" => {
            let value = source
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("image.source.url is required"))?;
            let parsed = Url::parse(value)
                .map_err(|_| AppError::bad_request("image.source.url must be a valid URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(AppError::bad_request(
                    "image.source.url must use http or https",
                ));
            }
            value.to_owned()
        }
        "file" | "file_id" => {
            return Err(AppError::bad_request(
                "Anthropic file_id images are unsupported; provide the image as Base64 or URL",
            ));
        }
        other => {
            return Err(AppError::bad_request(format!(
                "Unsupported image source type {other}; use base64 or url"
            )));
        }
    };
    Ok(json!({"type": "image_url", "image_url": {"url": url}}))
}

fn convert_assistant_blocks(
    blocks: &[Value],
    message_index: usize,
    ids: &mut IdContext,
    stripped_thinking: &mut bool,
) -> Result<Value, AppError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("thinking" | "redacted_thinking") => *stripped_thinking = true,
            Some("tool_use") => {
                let original = block.get("id").and_then(Value::as_str).unwrap_or("");
                let id = unique_tool_id(original, ids);
                ids.mappings
                    .entry(original.to_owned())
                    .or_default()
                    .push(id.clone());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(
                            block.get("input").unwrap_or(&Value::Object(Map::new()))
                        ).unwrap_or_else(|_| "{}".to_owned())
                    }
                }));
            }
            other => {
                return Err(AppError::bad_request(format!(
                    "messages[{message_index}].content[{block_index}].type: unsupported assistant content block {}",
                    other.unwrap_or("<missing>")
                )));
            }
        }
    }
    if tool_calls.is_empty() && text.is_empty() {
        return Err(AppError::bad_request(format!(
            "messages[{message_index}].content: assistant turn is empty after removing unsupported thinking blocks"
        )));
    }
    let mut message = Map::from_iter([
        ("role".into(), Value::String("assistant".into())),
        (
            "content".into(),
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            },
        ),
    ]);
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    Ok(Value::Object(message))
}

fn unique_tool_id(original: &str, ids: &mut IdContext) -> String {
    if !original.is_empty() && ids.seen.insert(original.to_owned()) {
        return original.to_owned();
    }
    let length = original.chars().count();
    let suffix_length = if original.is_empty() {
        24
    } else if length > 11 {
        length - 8
    } else {
        length
    };
    let suffix: String = (0..suffix_length)
        .map(|_| {
            const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            CHARS[rand::rng().random_range(0..CHARS.len())] as char
        })
        .collect();
    let repaired = if original.is_empty() {
        format!("toolu_{suffix}")
    } else if length > 11 {
        format!("{}{suffix}", original.chars().take(8).collect::<String>())
    } else {
        suffix
    };
    ids.seen.insert(repaired.clone());
    eprintln!("[adapter] Repair ID: {original} -> {repaired}");
    repaired
}

fn resolve_tool_result_id(original: &str, ids: &mut IdContext) -> String {
    let Some(mappings) = ids.mappings.get(original) else {
        return original.to_owned();
    };
    let index = ids.result_index.entry(original.to_owned()).or_default();
    let resolved = mappings
        .get(*index)
        .cloned()
        .unwrap_or_else(|| original.to_owned());
    *index += 1;
    resolved
}

fn convert_tool_choice(choice: &Value) -> Result<Value, AppError> {
    match choice.get("type").and_then(Value::as_str) {
        Some("none") => Ok(Value::String("none".into())),
        Some("auto") => Ok(Value::String("auto".into())),
        Some("any") => Ok(Value::String("required".into())),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| json!({"type": "function", "function": {"name": name}}))
            .ok_or_else(|| {
                AppError::bad_request(
                    "tool_choice.name: named tool choice requires a non-empty name",
                )
            }),
        Some(other) => Err(AppError::bad_request(format!(
            "tool_choice.type: unsupported tool choice {other}"
        ))),
        None => Err(AppError::bad_request(
            "tool_choice.type: tool choice type is required",
        )),
    }
}

fn convert_tools(tools: &[Value]) -> Result<Value, AppError> {
    let mut converted = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let tool = tool.as_object().ok_or_else(|| {
            AppError::bad_request(format!("tools[{index}]: tool must be an object"))
        })?;
        if tool.contains_key("allowed_callers") || tool.contains_key("defer_loading") {
            return Err(AppError::bad_request(format!(
                "tools[{index}]: allowed_callers and defer_loading are unsupported"
            )));
        }
        match tool.get("type") {
            None => {}
            Some(Value::String(kind)) if kind == "custom" => {}
            Some(Value::String(kind)) => {
                return Err(AppError::bad_request(format!(
                    "tools[{index}].type: Anthropic server tool {kind} is unsupported"
                )));
            }
            Some(_) => {
                return Err(AppError::bad_request(format!(
                    "tools[{index}].type: tool type must be custom when provided"
                )));
            }
        }
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                AppError::bad_request(format!(
                    "tools[{index}].name: name must be a non-empty string"
                ))
            })?;
        let schema = tool
            .get("input_schema")
            .filter(|schema| schema.is_object())
            .ok_or_else(|| {
                AppError::bad_request(format!(
                    "tools[{index}].input_schema: input_schema must be an object"
                ))
            })?;
        let mut function = Map::from_iter([
            ("name".into(), Value::String(name.to_owned())),
            (
                "description".into(),
                tool.get("description").cloned().unwrap_or(Value::Null),
            ),
            ("parameters".into(), schema.clone()),
        ]);
        if let Some(strict) = tool.get("strict") {
            function.insert("strict".into(), strict.clone());
        }
        converted.push(json!({"type": "function", "function": function}));
    }
    Ok(Value::Array(converted))
}

fn apply_model_options(
    request: &Map<String, Value>,
    output: &mut Map<String, Value>,
    model: &str,
) -> Result<(), AppError> {
    let thinking_type = request
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str);
    let effort = request
        .get("output_config")
        .and_then(|config| config.get("effort"))
        .and_then(Value::as_str);

    if is_openai_reasoning(model)
        && let Some(value) = openai_effort(effort, thinking_type, request)
    {
        output.insert("reasoning_effort".into(), Value::String(value.into()));
    }
    if is_glm5(model) {
        if let Some(kind) = thinking_type {
            output.insert(
                "thinking".into(),
                json!({"type": if kind == "disabled" { "disabled" } else { "enabled" }}),
            );
        }
        if request.get("stream").and_then(Value::as_bool) == Some(true)
            && request
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty())
        {
            output.insert("tool_stream".into(), Value::Bool(true));
        }
        if is_glm52(model)
            && thinking_type.is_some_and(|kind| kind != "disabled")
            && let Some(value) = glm_effort(effort, thinking_type)
        {
            output.insert("reasoning_effort".into(), Value::String(value.into()));
        }
    }
    if is_qwen3(model) && matches!(thinking_type, Some("enabled" | "adaptive")) {
        if request.get("stream").and_then(Value::as_bool) != Some(true) {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "Qwen thinking mode in this adapter requires stream=true because the upstream provider only supports it reliably on streaming calls.",
            ));
        }
        output.insert("enable_thinking".into(), Value::Bool(true));
    }
    Ok(())
}

fn openai_effort<'a>(
    effort: Option<&'a str>,
    thinking_type: Option<&str>,
    request: &Map<String, Value>,
) -> Option<&'a str> {
    if let Some(effort) = effort {
        return Some(match effort {
            "max" => "xhigh",
            other => other,
        });
    }
    match thinking_type {
        Some("adaptive") => Some("xhigh"),
        Some("enabled") => Some(
            match request
                .get("thinking")
                .and_then(|thinking| thinking.get("budget_tokens"))
                .and_then(Value::as_u64)
            {
                Some(value) if value < 4_000 => "low",
                Some(value) if value < 16_000 => "medium",
                _ => "high",
            },
        ),
        _ => None,
    }
}

fn glm_effort(effort: Option<&str>, thinking_type: Option<&str>) -> Option<&'static str> {
    match effort {
        Some("max") => Some("max"),
        Some("high" | "medium" | "low") => Some("high"),
        _ if thinking_type == Some("adaptive") => Some("max"),
        _ => None,
    }
}

pub fn convert_response(response: &Value, original_model: &str) -> Result<Value, AppError> {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| upstream_protocol_error("response is missing choices[0]"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| upstream_protocol_error("response is missing choices[0].message"))?;
    let mut content = Vec::new();
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .unwrap_or("");
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
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    upstream_protocol_error("tool call is missing function.arguments")
                })?;
            let input: Value = serde_json::from_str(arguments).map_err(|error| {
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
    let filtered = finish == "content_filter";
    let refusal_explanation =
        refusal.unwrap_or("Response withheld by the upstream content filter.");
    if filtered && content.is_empty() {
        content.push(json!({"type": "text", "text": refusal_explanation}));
    }
    if !reasoning.is_empty() && content.is_empty() {
        return Err(upstream_protocol_error(
            "upstream response contained reasoning but no user-visible content",
        ));
    }
    let is_refusal = filtered || refusal.is_some();
    let stop_reason = if is_refusal { "refusal" } else { mapped_finish };
    let stop_details = if is_refusal {
        json!({"type": "refusal", "category": Value::Null, "explanation": refusal_explanation})
    } else {
        Value::Null
    };
    let usage = response.get("usage");
    Ok(json!({
        "id": format!("msg_{}", response.get("id").and_then(Value::as_str).unwrap_or("")),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": original_model,
        "stop_reason": stop_reason,
        "stop_details": stop_details,
        "stop_sequence": Value::Null,
        "container": Value::Null,
        "usage": response_usage(usage)
    }))
}

pub(crate) fn map_finish_reason(reason: &str) -> Result<&'static str, AppError> {
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

pub(crate) fn thinking_tokens(usage: Option<&Value>) -> Option<u64> {
    usage
        .and_then(|value| value.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(Value::as_u64)
}

fn response_usage(usage: Option<&Value>) -> Value {
    json!({
        "input_tokens": usage.and_then(|value| value.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0),
        "output_tokens": usage.and_then(|value| value.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0),
        "cache_creation_input_tokens": Value::Null,
        "cache_read_input_tokens": Value::Null,
        "output_tokens_details": thinking_tokens(usage).map(|tokens| json!({"thinking_tokens": tokens})),
        "server_tool_use": Value::Null,
        "cache_creation": Value::Null,
        "inference_geo": Value::Null,
        "service_tier": Value::Null,
    })
}

fn upstream_protocol_error(message: impl Into<String>) -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, message)
}

fn normalized(model: &str) -> String {
    model.trim().to_lowercase()
}
fn is_openai_reasoning(model: &str) -> bool {
    let model = normalized(model);
    model.starts_with("gpt-5")
        || (model.starts_with('o') && model.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
}
fn is_glm5(model: &str) -> bool {
    normalized(model).starts_with("glm-5")
}
fn is_glm52(model: &str) -> bool {
    normalized(model).starts_with("glm-5.2")
}
fn is_qwen3(model: &str) -> bool {
    normalized(model).starts_with("qwen3")
}
fn is_azure(base_url: &str) -> bool {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .is_some_and(|host| {
            host.ends_with(".openai.azure.com") || host.contains(".services.ai.azure.com")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_shared_request_fixtures() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../bench/fixtures/request-conversion.json"))
                .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let actual = convert_request(&fixture["input"], "https://api.openai.com/v1").unwrap();
            assert_eq!(actual, fixture["expected"], "fixture {}", fixture["name"]);
        }
    }

    #[test]
    fn rejects_empty_required_strings() {
        for body in [
            json!({"model": "", "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}]}),
            json!({"model": "model", "max_tokens": 1, "messages": [{"role": "", "content": "ok"}]}),
            json!({"model": "model", "max_tokens": 1, "messages": [{"role": "user", "content": [{"type": "", "text": "ok"}]}]}),
        ] {
            assert!(validate_request(&body).is_err());
        }
    }

    #[test]
    fn validates_request_roles_and_cache_only_limit() {
        let invalid_role = json!({
            "model": "gpt-4",
            "max_tokens": 1,
            "messages": [{"role": "system", "content": "hidden"}]
        });
        let error = validate_request(&invalid_role).unwrap_err();
        assert!(error.message.contains("messages[0].role"));

        let cache_only = json!({
            "model": "gpt-4",
            "max_tokens": 0,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let error = validate_request(&cache_only).unwrap_err();
        assert!(error.message.contains("cache-only"));
    }

    #[test]
    fn preserves_prefill_and_maps_tool_controls() {
        let body = json!({
            "model": "gpt-4",
            "max_tokens": 10,
            "messages": [{"role": "assistant", "content": "{"}],
            "tools": [{
                "type": "custom",
                "name": "lookup",
                "description": "Lookup a value",
                "input_schema": {"type": "object"},
                "strict": true,
                "cache_control": {"type": "ephemeral"}
            }],
            "tool_choice": {
                "type": "tool",
                "name": "lookup",
                "disable_parallel_tool_use": true
            }
        });
        validate_request(&body).unwrap();
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][0]["content"], "{");
        assert_eq!(converted["tools"][0]["function"]["strict"], true);
        assert_eq!(converted["tool_choice"]["function"]["name"], "lookup");
        assert_eq!(converted["parallel_tool_calls"], false);
    }

    #[test]
    fn rejects_unsupported_tools_and_content_blocks() {
        for body in [
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "web_search_20250305", "name": "web", "input_schema": {}}]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": 1, "name": "lookup", "input_schema": {}}]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": [{"type": "document", "source": {}}]}]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "toolu_1",
                    "content": [{"type": "search_result", "content": []}]
                }]}]
            }),
        ] {
            validate_request(&body).unwrap();
            assert!(convert_request(&body, "https://api.openai.com/v1").is_err());
        }
    }

    #[test]
    fn strips_thinking_history_only_when_visible_content_remains() {
        let valid = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "private"},
                {"type": "text", "text": "visible"}
            ]}]
        });
        let converted = convert_request(&valid, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][0]["content"], "visible");

        let empty = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [{"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "private"}
            ]}]
        });
        assert!(convert_request(&empty, "https://api.openai.com/v1").is_err());
    }

    #[test]
    fn converts_text_and_base64_images_in_order() {
        let body = json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "before"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aQ=="}},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/a.webp"}},
                {"type": "text", "text": "after"}
            ]}]
        });
        validate_request(&body).unwrap();
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][0]["content"][0]["text"], "before");
        assert_eq!(
            converted["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,aQ=="
        );
        assert_eq!(
            converted["messages"][0]["content"][2]["image_url"]["url"],
            "https://example.com/a.webp"
        );
        assert_eq!(converted["messages"][0]["content"][3]["text"], "after");
    }

    #[test]
    fn moves_tool_result_images_after_text_tool_messages() {
        let body = json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [
                {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "shot", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}
                ]}]}
            ]
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][1]["role"], "tool");
        assert_eq!(converted["messages"][2]["role"], "user");
        assert_eq!(
            converted["messages"][2]["content"][1]["image_url"]["url"],
            "https://example.com/a.png"
        );
    }

    #[test]
    fn preserves_parallel_image_tool_results_and_error_associations() {
        let body = json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "first", "input": {}},
                    {"type": "tool_use", "id": "toolu_2", "name": "second", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "text", "text": "first text"},
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/1.png"}},
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/2.png"}}
                    ]},
                    {"type": "tool_result", "tool_use_id": "toolu_2", "is_error": true, "content": [
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/error.png"}}
                    ]}
                ]}
            ]
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][1]["content"], "first text");
        assert_eq!(
            converted["messages"][2]["content"],
            "Error: Image result follows in the next user message."
        );
        let images = converted["messages"][3]["content"].as_array().unwrap();
        assert_eq!(
            images[0]["text"],
            "Tool result images for tool_call_id=toolu_1:"
        );
        assert_eq!(
            images[3]["text"],
            "Error: Tool result images for tool_call_id=toolu_2:"
        );
        assert_eq!(
            images[4]["image_url"]["url"],
            "https://example.com/error.png"
        );
    }

    #[test]
    fn repairs_duplicate_tool_ids_and_matches_results_in_order() {
        let body = json!({
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "duplicate_tool_id", "name": "first", "input": {}},
                    {"type": "tool_use", "id": "duplicate_tool_id", "name": "second", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "duplicate_tool_id", "content": "one"},
                    {"type": "tool_result", "tool_use_id": "duplicate_tool_id", "content": "two"}
                ]}
            ]
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        let calls = converted["messages"][0]["tool_calls"].as_array().unwrap();
        assert_ne!(calls[0]["id"], calls[1]["id"]);
        assert_eq!(converted["messages"][1]["tool_call_id"], calls[0]["id"]);
        assert_eq!(converted["messages"][2]["tool_call_id"], calls[1]["id"]);
    }

    #[test]
    fn rejects_invalid_image_sources() {
        for source in [
            json!({"type": "base64", "media_type": "image/png", "data": "%%%"}),
            json!({"type": "base64", "media_type": "image/svg+xml", "data": "PHN2Zz4="}),
            json!({"type": "url", "url": "ftp://example.com/a.png"}),
            json!({"type": "url", "url": "not a url"}),
            json!({"type": "url"}),
            json!({"type": "file_id", "file_id": "file_1"}),
        ] {
            let body = json!({
                "model": "gpt-4o",
                "max_tokens": 100,
                "messages": [{"role": "user", "content": [{"type": "image", "source": source}]}]
            });
            assert!(convert_request(&body, "https://api.openai.com/v1").is_err());
        }
    }

    #[test]
    fn matches_shared_response_fixtures() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../bench/fixtures/response-conversion.json"
        ))
        .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let actual =
                convert_response(&fixture["input"], fixture["model"].as_str().unwrap()).unwrap();
            assert_eq!(actual, fixture["expected"], "fixture {}", fixture["name"]);
        }
    }

    #[test]
    fn maps_finish_reasons_and_current_usage_shape() {
        for (finish, expected) in [
            ("stop", "end_turn"),
            ("length", "max_tokens"),
            ("tool_calls", "tool_use"),
        ] {
            let response = json!({
                "id": "chatcmpl_1",
                "choices": [{"message": {"content": "ok"}, "finish_reason": finish}],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 7,
                    "completion_tokens_details": {"reasoning_tokens": 3}
                }
            });
            let converted = convert_response(&response, "model").unwrap();
            assert_eq!(converted["stop_reason"], expected);
            assert_eq!(converted["stop_details"], Value::Null);
            assert_eq!(converted["container"], Value::Null);
            assert_eq!(converted["usage"]["input_tokens"], 11);
            assert_eq!(converted["usage"]["output_tokens"], 7);
            assert_eq!(
                converted["usage"]["output_tokens_details"]["thinking_tokens"],
                3
            );
            assert_eq!(converted["usage"]["cache_read_input_tokens"], Value::Null);
        }
    }

    #[test]
    fn preserves_refusal_without_inventing_a_content_block_type() {
        let response = json!({
            "id": "chatcmpl_1",
            "choices": [{
                "message": {"content": "partial", "refusal": "Cannot comply."},
                "finish_reason": "content_filter"
            }]
        });
        let converted = convert_response(&response, "model").unwrap();
        assert_eq!(converted["stop_reason"], "refusal");
        assert_eq!(converted["stop_details"]["explanation"], "Cannot comply.");
        assert_eq!(
            converted["content"][0],
            json!({"type": "text", "text": "partial"})
        );
        assert_eq!(
            converted["content"][1],
            json!({"type": "text", "text": "Cannot comply."})
        );

        let filtered = json!({
            "id": "chatcmpl_2",
            "choices": [{"message": {"content": null}, "finish_reason": "content_filter"}]
        });
        let converted = convert_response(&filtered, "model").unwrap();
        assert_eq!(
            converted["content"][0]["text"],
            "Response withheld by the upstream content filter."
        );
    }

    #[test]
    fn rejects_reasoning_only_and_malformed_tool_calls() {
        for response in [
            json!({
                "id": "chatcmpl_1",
                "choices": [{"message": {"reasoning_content": "private"}, "finish_reason": "stop"}]
            }),
            json!({
                "id": "chatcmpl_2",
                "choices": [{"message": {"tool_calls": [{
                    "id": "toolu_1", "function": {"name": "lookup", "arguments": "not json"}
                }]}, "finish_reason": "tool_calls"}]
            }),
            json!({
                "id": "chatcmpl_3",
                "choices": [{"message": {"tool_calls": [{
                    "id": "toolu_1", "function": {"name": "lookup", "arguments": "[]"}
                }]}, "finish_reason": "tool_calls"}]
            }),
            json!({
                "id": "chatcmpl_4",
                "choices": [{"message": {"tool_calls": [{
                    "id": "toolu_1", "function": {"arguments": "{}"}
                }]}, "finish_reason": "tool_calls"}]
            }),
        ] {
            let error = convert_response(&response, "model").unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        }
    }

    #[test]
    fn rejects_unknown_finish_reason() {
        for reason in ["function_call", "provider_magic"] {
            let response = json!({
                "id": "chatcmpl_1",
                "choices": [{"message": {"content": "ok"}, "finish_reason": reason}]
            });
            assert!(convert_response(&response, "model").is_err());
        }
    }
}

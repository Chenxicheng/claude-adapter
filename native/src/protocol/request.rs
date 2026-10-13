use std::{
    collections::{HashMap, HashSet},
    io,
};

use axum::http::StatusCode;
use base64::{engine::general_purpose::STANDARD, read::DecoderReader};
use rand::Rng;
use serde_json::{Map, Value, json};
use url::Url;

use crate::{config::AssistantPrefillMode, error::AppError};

const BILLING_HEADER: &str = "x-anthropic-billing-header:";
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
    match object.get("max_tokens").and_then(Value::as_u64) {
        None => errors.push("max_tokens: max_tokens is required and must be a number".to_owned()),
        Some(0) => errors.push(
            "max_tokens: cache-only requests are unsupported; max_tokens must be positive"
                .to_owned(),
        ),
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
    validate_system(object.get("system"), &mut errors);
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
        let role = message.get("role").and_then(Value::as_str);
        if !matches!(role, Some("user" | "assistant" | "system")) {
            errors.push(format!(
                "messages[{message_index}].role: role must be user, assistant, or system"
            ));
        }
        if role == Some("system") {
            if message
                .get("clear_at")
                .is_some_and(|value| !value.is_null() && value != "never")
            {
                errors.push(format!(
                    "messages[{message_index}].clear_at: not supported by Chat Completions conversion"
                ));
            }
            if message
                .get("output_config")
                .is_some_and(|value| !value.is_null())
            {
                errors.push(format!(
                    "messages[{message_index}].output_config: not supported by Chat Completions conversion"
                ));
            }
        }
        match message.get("content") {
            None | Some(Value::Null) => {}
            Some(content) => match content {
                Value::String(_) => {}
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
                            Some(block) => validate_content_block(role, block, &field, errors),
                        }
                    }
                }
                _ => errors.push(format!(
                    "messages[{message_index}].content: content must be a string or array"
                )),
            },
        }
    }
}

fn validate_content_block(
    role: Option<&str>,
    block: &Map<String, Value>,
    field: &str,
    errors: &mut Vec<String>,
) {
    let Some(kind) = block.get("type").and_then(Value::as_str) else {
        return;
    };
    match (role == Some("user"), kind) {
        (_, "text") => validate_text_block(block, field, errors),
        (true, "image") => validate_image_block(block, field, errors),
        (_, "image") => errors.push(format!("{field}: image blocks require a user message")),
        (true, "tool_result") => validate_tool_result(block, field, errors),
        (false, "tool_use") if role == Some("assistant") => validate_tool_use(block, field, errors),
        (false, "thinking")
            if role == Some("assistant")
                && block.get("thinking").and_then(Value::as_str).is_none() =>
        {
            errors.push(format!("{field}.thinking: thinking must be a string"));
        }
        (false, "thinking" | "redacted_thinking") if role == Some("assistant") => {}
        _ => errors.push(format!("{field}.type: unsupported content block type")),
    }
}

fn validate_text_block(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if block.get("text").and_then(Value::as_str).is_none() {
        errors.push(format!(
            "{field}.text: text is required and must be a string"
        ));
    }
}

fn validate_image_block(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    let Some(source) = block.get("source").and_then(Value::as_object) else {
        errors.push(format!("{field}.source: image source must be an object"));
        return;
    };
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            if !matches!(
                source.get("media_type").and_then(Value::as_str),
                Some("image/jpeg" | "image/png" | "image/gif" | "image/webp")
            ) {
                errors.push(format!(
                    "{field}.source.media_type: unsupported image media type"
                ));
            }
            let data = source.get("data").and_then(Value::as_str);
            if data.is_none_or(str::is_empty)
                || io::copy(
                    &mut DecoderReader::new(data.unwrap_or_default().as_bytes(), &STANDARD),
                    &mut io::sink(),
                )
                .is_err()
            {
                errors.push(format!("{field}.source.data: invalid base64 image data"));
            }
        }
        Some("url") => {
            let valid = source
                .get("url")
                .and_then(Value::as_str)
                .and_then(|value| Url::parse(value).ok())
                .is_some_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host().is_some()
                });
            if !valid {
                errors.push(format!(
                    "{field}.source.url: image URL must be HTTP or HTTPS"
                ));
            }
        }
        _ => errors.push(format!(
            "{field}.source.type: unsupported image source type"
        )),
    }
}

fn convert_image_block(block: &Value) -> Value {
    let source = &block["source"];
    let url = if source["type"] == "base64" {
        format!(
            "data:{};base64,{}",
            source["media_type"].as_str().expect("validated media type"),
            source["data"].as_str().expect("validated image data")
        )
    } else {
        source["url"]
            .as_str()
            .expect("validated image URL")
            .to_owned()
    };
    json!({"type": "image_url", "image_url": {"url": url}})
}

fn validate_tool_result(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if block
        .get("content")
        .is_some_and(|content| !content.is_null() && !content.is_string() && !content.is_array())
    {
        errors.push(format!("{field}.content: must be a string or array"));
    }
    if block
        .get("tool_use_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push(format!(
            "{field}.tool_use_id: tool_use_id must be a non-empty string"
        ));
    }
    if block
        .get("is_error")
        .is_some_and(|value| !value.is_boolean())
    {
        errors.push(format!("{field}.is_error: is_error must be a boolean"));
    }
    if let Some(Value::Array(parts)) = block.get("content") {
        for (index, part) in parts.iter().enumerate() {
            let part_field = format!("{field}.content[{index}]");
            let Some(part) = part.as_object() else {
                errors.push(format!("{part_field}: content block must be an object"));
                continue;
            };
            match part.get("type").and_then(Value::as_str) {
                Some("text") => validate_text_block(part, &part_field, errors),
                Some("image") => validate_image_block(part, &part_field, errors),
                Some(_) => errors.push(format!(
                    "{part_field}.type: unsupported tool result block type"
                )),
                None => errors.push(format!("{part_field}.type: content block type is required")),
            }
        }
    }
}

fn validate_tool_use(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if block
        .get("id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push(format!("{field}.id: id must be a non-empty string"));
    }
    if block
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push(format!("{field}.name: name must be a non-empty string"));
    }
    if block.get("input").is_none_or(|input| !input.is_object()) {
        errors.push(format!("{field}.input: input must be an object"));
    }
}

fn validate_system(system: Option<&Value>, errors: &mut Vec<String>) {
    let Some(system) = system else {
        return;
    };
    match system {
        Value::String(_) => {}
        Value::Array(blocks) => {
            for (index, block) in blocks.iter().enumerate() {
                let field = format!("system[{index}]");
                let Some(block) = block.as_object() else {
                    errors.push(format!("{field}: system block must be an object"));
                    continue;
                };
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    errors.push(format!("{field}.type: system block type must be text"));
                    continue;
                }
                validate_text_block(block, &field, errors);
            }
        }
        _ => errors.push("system: system must be a string or array of text blocks".to_owned()),
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

pub fn convert_request_with_mode(
    body: &Value,
    base_url: &str,
    assistant_prefill_mode: AssistantPrefillMode,
) -> Result<Value, AppError> {
    validate_request(body)?;
    let request = body.as_object().expect("validated request object");
    for field in ["top_k", "service_tier", "mcp_servers", "container"] {
        if request.get(field).is_some_and(|value| !value.is_null()) {
            return Err(AppError::bad_request(format!(
                "{field}: not supported by Chat Completions conversion"
            )));
        }
    }
    if request
        .get("output_config")
        .and_then(|value| value.get("format"))
        .is_some_and(|value| !value.is_null())
    {
        return Err(AppError::bad_request(
            "output_config.format: structured output conversion is not supported",
        ));
    }
    let model = request["model"].as_str().expect("validated model");
    let source_messages = request["messages"].as_array().expect("validated messages");
    let terminal_assistant_prefill = source_messages.last().is_some_and(has_assistant_prefill);
    if terminal_assistant_prefill && assistant_prefill_mode == AssistantPrefillMode::Unsupported {
        return Err(AppError::bad_request(
            "Terminal assistant prefill is not supported by the configured upstream. Set upstreamCapabilities.assistantPrefill to \"continue_final_message\" only for an upstream that implements that extension, or to \"native\" only for an upstream that natively continues a final assistant message",
        ));
    }
    let mut messages = Vec::new();

    if let Some(system) = request.get("system") {
        let content = system_content(system);
        let content = strip_billing_header(&content);
        if !content.is_empty() {
            messages.push(json!({"role": "system", "content": content}));
        }
    }

    let mut ids = IdContext::default();
    let preserve_reasoning = is_glm5(model) || is_qwen3(model);
    for (message_index, message) in source_messages.iter().enumerate() {
        messages.extend(convert_message(
            message,
            message_index,
            &mut ids,
            preserve_reasoning,
        )?);
    }
    if messages.is_empty() {
        return Err(AppError::bad_request(
            "No messages after conversion: all input messages had missing content",
        ));
    }
    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_owned()));
    output.insert("messages".into(), Value::Array(messages));
    output.insert(
        "stream".into(),
        Value::Bool(request.get("stream").and_then(Value::as_bool) == Some(true)),
    );
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
    if let Some(stop) = request.get("stop_sequences").and_then(Value::as_array) {
        output.insert("stop".into(), Value::Array(stop.clone()));
    }
    let tools = request
        .get("tools")
        .and_then(Value::as_array)
        .filter(|tools| !tools.is_empty());
    if let Some(tools) = tools {
        output.insert("tools".into(), convert_tools(tools)?);
    }
    if let Some(choice) = request.get("tool_choice") {
        output.insert("tool_choice".into(), convert_tool_choice(choice)?);
        if let Some(disabled) = choice.get("disable_parallel_tool_use") {
            let disabled = disabled.as_bool().ok_or_else(|| {
                AppError::bad_request("tool_choice.disable_parallel_tool_use: must be a boolean")
            })?;
            output.insert("parallel_tool_calls".into(), Value::Bool(!disabled));
        }
    }
    apply_model_options(request, &mut output, model)?;
    if terminal_assistant_prefill
        && assistant_prefill_mode == AssistantPrefillMode::ContinueFinalMessage
    {
        output.insert("continue_final_message".into(), Value::Bool(true));
    }
    Ok(Value::Object(output))
}

fn has_assistant_prefill(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    match message.get("content") {
        Some(Value::String(content)) => !content.is_empty(),
        Some(Value::Array(blocks)) => blocks.iter().any(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
        }),
        _ => false,
    }
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
        _ => unreachable!("validated system content"),
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
pub(super) struct IdContext {
    seen: HashSet<String>,
    mappings: HashMap<String, Vec<String>>,
    result_index: HashMap<String, usize>,
}

pub(super) fn unique_tool_id(original: &str, ids: &mut IdContext) -> String {
    if !original.is_empty() && ids.seen.insert(original.to_owned()) {
        return original.to_owned();
    }
    let length = original.encode_utf16().count();
    // Rust strings cannot retain the lone surrogate produced by JS substring at a split pair.
    let prefix = (length > 11).then(|| utf16_prefix(original, 8));
    let suffix_length = if original.is_empty() {
        24
    } else if let Some(prefix) = prefix {
        length - prefix.encode_utf16().count()
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
    } else if let Some(prefix) = prefix {
        format!("{prefix}{suffix}")
    } else {
        suffix
    };
    ids.seen.insert(repaired.clone());
    eprintln!("[adapter] Repair ID: {original} -> {repaired}");
    repaired
}

fn utf16_prefix(value: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, character) in value.char_indices() {
        units += character.len_utf16();
        if units > max_units {
            return &value[..index];
        }
    }
    value
}

fn resolve_tool_result_id(original: &str, ids: &mut IdContext) -> Option<String> {
    let mappings = ids.mappings.get(original)?;
    let index = ids.result_index.entry(original.to_owned()).or_default();
    let resolved = mappings.get(*index).cloned()?;
    *index += 1;
    Some(resolved)
}

fn convert_message(
    message: &Value,
    message_index: usize,
    ids: &mut IdContext,
    preserve_reasoning: bool,
) -> Result<Vec<Value>, AppError> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let Some(content) = message.get("content").filter(|content| !content.is_null()) else {
        return Ok(Vec::new());
    };
    if let Some(content) = content.as_str() {
        return Ok(vec![json!({"role": role, "content": content})]);
    }

    let blocks = content.as_array().expect("validated content array");
    match role {
        "user" => convert_user_blocks(blocks, message_index, ids),
        "system" => Ok(vec![
            json!({"role": "system", "content": system_content(content)}),
        ]),
        "assistant" => {
            let converted = convert_assistant_blocks(blocks, ids, preserve_reasoning)?;
            Ok(vec![converted])
        }
        _ => unreachable!("validated message role"),
    }
}

fn convert_user_blocks(
    blocks: &[Value],
    _message_index: usize,
    ids: &mut IdContext,
) -> Result<Vec<Value>, AppError> {
    let mut tool_messages = Vec::new();
    let mut user_parts = Vec::new();

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => user_parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).expect("validated text")
            })),
            Some("image") => user_parts.push(convert_image_block(block)),
            Some("tool_result") => {
                let original_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .expect("validated tool_use_id");
                let resolved_id = resolve_tool_result_id(original_id, ids)
                    .unwrap_or_else(|| original_id.to_owned());
                let (text, images) = extract_tool_result(block);
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let mut text = if is_error {
                    format!("Error: {text}")
                } else {
                    text
                };
                if !images.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("[Image output follows in the next user message.]");
                    user_parts.push(json!({"type": "text", "text": format!(
                        "[Image from tool_call_id: {resolved_id}]"
                    )}));
                    user_parts.extend(images);
                }
                tool_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": resolved_id,
                    "content": text,
                }));
            }
            _ => {}
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
    Ok(result)
}

fn extract_tool_result(block: &Value) -> (String, Vec<Value>) {
    match block.get("content") {
        Some(Value::String(text)) => (text.clone(), Vec::new()),
        Some(Value::Array(parts)) => {
            let mut text = Vec::new();
            let mut images = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => text.push(
                        part.get("text")
                            .and_then(Value::as_str)
                            .expect("validated text")
                            .to_owned(),
                    ),
                    Some("image") => images.push(convert_image_block(part)),
                    _ => {}
                }
            }
            (text.join("\n"), images)
        }
        None | Some(Value::Null) => (String::new(), Vec::new()),
        Some(_) => (String::new(), Vec::new()),
    }
}

fn convert_assistant_blocks(
    blocks: &[Value],
    ids: &mut IdContext,
    preserve_reasoning: bool,
) -> Result<Value, AppError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .expect("validated text"),
            ),
            Some("thinking") => reasoning.push_str(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .expect("validated thinking"),
            ),
            Some("redacted_thinking") => {}
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
                        "name": block.get("name").and_then(Value::as_str).expect("validated tool name"),
                        "arguments": serde_json::to_string(
                            block.get("input").expect("validated tool input")
                        ).expect("JSON value serialization")
                    }
                }));
            }
            _ => {}
        }
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
        if preserve_reasoning && !reasoning.is_empty() {
            message.insert("reasoning_content".into(), Value::String(reasoning));
        }
    }
    Ok(Value::Object(message))
}

fn convert_tool_choice(choice: &Value) -> Result<Value, AppError> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(Value::String("auto".into())),
        Some("any") => Ok(Value::String("required".into())),
        Some("none") => Ok(Value::String("none".into())),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| json!({"type": "function", "function": {"name": name}}))
            .ok_or_else(|| AppError::bad_request("tool_choice.name: required for a named tool")),
        _ => Err(AppError::bad_request(
            "tool_choice.type: unsupported tool choice",
        )),
    }
}

fn convert_tools(tools: &[Value]) -> Result<Value, AppError> {
    let mut converted = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let tool = tool.as_object().ok_or_else(|| {
            AppError::bad_request(format!("tools[{index}]: tool must be an object"))
        })?;
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
        if tool
            .get("strict")
            .is_some_and(|strict| !strict.is_null() && strict != &Value::Bool(false))
        {
            return Err(AppError::bad_request(format!(
                "tools[{index}].strict: strict tool schemas are not supported by this conversion"
            )));
        }
        let mut function = Map::from_iter([
            ("name".into(), Value::String(name.to_owned())),
            ("parameters".into(), schema.clone()),
        ]);
        if let Some(description) = tool.get("description") {
            function.insert("description".into(), description.clone());
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
        return Ok(());
    }
    if is_qwen3(model) {
        if matches!(thinking_type, Some("enabled" | "adaptive")) {
            if request.get("stream").and_then(Value::as_bool) != Some(true) {
                return Err(AppError::new(
                    StatusCode::BAD_REQUEST,
                    "Qwen thinking mode in this adapter requires stream=true because the upstream provider only supports it reliably on streaming calls.",
                ));
            }
            output.insert("enable_thinking".into(), Value::Bool(true));
        }
        return Ok(());
    }
    if is_openai_reasoning(model)
        && let Some(effort) = openai_effort(request, effort, thinking_type)
    {
        output.insert("reasoning_effort".into(), Value::String(effort.to_owned()));
    }
    Ok(())
}

fn openai_effort(
    request: &Map<String, Value>,
    effort: Option<&str>,
    thinking_type: Option<&str>,
) -> Option<&'static str> {
    match effort {
        Some("low") => return Some("low"),
        Some("medium") => return Some("medium"),
        Some("high") => return Some("high"),
        Some("xhigh") => return Some("xhigh"),
        Some("max") => return Some("xhigh"),
        Some(_) => return None,
        None => {}
    }
    match thinking_type {
        Some("adaptive") => Some("xhigh"),
        Some("enabled") => match request
            .get("thinking")
            .and_then(|thinking| thinking.get("budget_tokens"))
            .and_then(Value::as_u64)
        {
            None => Some("high"),
            Some(budget) if budget < 4_000 => Some("low"),
            Some(budget) if budget < 16_000 => Some("medium"),
            Some(_) => Some("high"),
        },
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
    normalized(model)
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("qwen3"))
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
#[path = "../../tests/unit/protocol/request.rs"]
mod tests;

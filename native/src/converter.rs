use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use rand::Rng;
use serde_json::{Map, Value, json};
use url::Url;

use crate::error::AppError;

const BILLING_HEADER: &str = "x-anthropic-billing-header:";
const ASSISTANT_PREFILL_TOKENS: &[&str] = &["{", "[", "```", "{\"", "[{"];
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
        match role {
            Some(role) if !role.is_empty() => {}
            None => errors.push(format!(
                "messages[{message_index}].role: role is required and must be a string"
            )),
            Some(_) => errors.push(format!(
                "messages[{message_index}].role: role is required and must be a string"
            )),
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
        (_, "image") => validate_image_block(block, field, errors),
        (true, "tool_result") => validate_tool_result(block, field, errors),
        (false, "tool_use") => validate_tool_use(block, field, errors),
        (false, "thinking") if block.get("thinking").and_then(Value::as_str).is_none() => {
            errors.push(format!("{field}.thinking: thinking must be a string"));
        }
        _ => {}
    }
}

fn validate_text_block(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if block.get("text").and_then(Value::as_str).is_none() {
        errors.push(format!(
            "{field}.text: text is required and must be a string"
        ));
    }
}

fn validate_image_block(_block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    errors.push(format!("{field}: image input is not supported"));
}

fn validate_tool_result(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
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
                Some(_) => {}
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

pub fn convert_request(body: &Value, base_url: &str) -> Result<Value, AppError> {
    validate_request(body)?;
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
    let preserve_reasoning = is_glm5(model) || is_qwen3(model);
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
struct IdContext {
    seen: HashSet<String>,
    mappings: HashMap<String, Vec<String>>,
    result_index: HashMap<String, usize>,
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
        if role != "user" && is_assistant_prefill(content) {
            return Ok(Vec::new());
        }
        let role = if role == "user" { "user" } else { "assistant" };
        return Ok(vec![json!({"role": role, "content": content})]);
    }

    let blocks = content.as_array().expect("validated content array");
    match role {
        "user" => convert_user_blocks(blocks, message_index, ids),
        _ => {
            let converted = convert_assistant_blocks(blocks, ids, preserve_reasoning)?;
            if converted.get("tool_calls").is_none()
                && converted
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(is_assistant_prefill)
            {
                Ok(Vec::new())
            } else {
                Ok(vec![converted])
            }
        }
    }
}

fn is_assistant_prefill(content: &str) -> bool {
    let trimmed = content.trim();
    ASSISTANT_PREFILL_TOKENS.contains(&trimmed) || trimmed.encode_utf16().count() <= 2
}

fn convert_user_blocks(
    blocks: &[Value],
    message_index: usize,
    ids: &mut IdContext,
) -> Result<Vec<Value>, AppError> {
    let mut tool_messages = Vec::new();
    let mut user_parts = Vec::new();

    for (block_index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => user_parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).expect("validated text")
            })),
            Some("image") => {
                return Err(AppError::bad_request(format!(
                    "messages[{message_index}].content[{block_index}]: image input is not supported"
                )));
            }
            Some("tool_result") => {
                let original_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .expect("validated tool_use_id");
                let resolved_id = resolve_tool_result_id(original_id, ids)
                    .unwrap_or_else(|| original_id.to_owned());
                let text = extract_tool_result(block, message_index, block_index)?;
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
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

fn extract_tool_result(
    block: &Value,
    message_index: usize,
    block_index: usize,
) -> Result<String, AppError> {
    match block.get("content") {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => {
            let mut text = Vec::new();
            for (part_index, part) in parts.iter().enumerate() {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => text.push(
                        part.get("text")
                            .and_then(Value::as_str)
                            .expect("validated text")
                            .to_owned(),
                    ),
                    Some("image") => {
                        return Err(AppError::bad_request(format!(
                            "messages[{message_index}].content[{block_index}].content[{part_index}]: image input is not supported"
                        )));
                    }
                    _ => {}
                }
            }
            Ok(text.join("\n"))
        }
        None | Some(Value::Null) => Ok(String::new()),
        Some(_) => Ok(String::new()),
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

fn unique_tool_id(original: &str, ids: &mut IdContext) -> String {
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

fn convert_tool_choice(choice: &Value) -> Result<Value, AppError> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(Value::String("auto".into())),
        Some("any") => Ok(Value::String("required".into())),
        Some("tool") => Ok(choice
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| json!({"type": "function", "function": {"name": name}}))
            .unwrap_or_else(|| Value::String("auto".into()))),
        _ => Ok(Value::String("auto".into())),
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
    if (finish == "tool_calls") != has_tools {
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
    let usage = response_usage(response.get("usage"))?;
    let id = match response.get("id") {
        Some(Value::String(id)) if !id.is_empty() => format!("msg_{id}"),
        None | Some(Value::Null) | Some(Value::String(_)) => generated_message_id(),
        Some(_) => return Err(upstream_protocol_error("response id must be a string")),
    };
    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": original_model,
        "stop_reason": stop_reason,
        "stop_details": stop_details,
        "stop_sequence": Value::Null,
        "container": Value::Null,
        "usage": usage
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

fn response_usage(usage: Option<&Value>) -> Result<Value, AppError> {
    let usage = match usage {
        None | Some(Value::Null) => None,
        Some(Value::Object(usage)) => Some(usage),
        Some(_) => return Err(upstream_protocol_error("response usage must be an object")),
    };
    let token = |field: &str| -> Result<Value, AppError> {
        match usage.and_then(|usage| usage.get(field)) {
            None | Some(Value::Null) => Ok(Value::Null),
            Some(value) => value.as_u64().map(Value::from).ok_or_else(|| {
                upstream_protocol_error(format!("response usage.{field} must be an integer"))
            }),
        }
    };
    let details = match usage.and_then(|usage| usage.get("completion_tokens_details")) {
        None | Some(Value::Null) => None,
        Some(Value::Object(details)) => Some(details),
        Some(_) => {
            return Err(upstream_protocol_error(
                "response usage.completion_tokens_details must be an object or null",
            ));
        }
    };
    let thinking = match details.and_then(|details| details.get("reasoning_tokens")) {
        None | Some(Value::Null) => Value::Null,
        Some(value) => value
            .as_u64()
            .map(|tokens| json!({"thinking_tokens": tokens}))
            .ok_or_else(|| {
                upstream_protocol_error(
                    "response usage.completion_tokens_details.reasoning_tokens must be an integer",
                )
            })?,
    };
    Ok(json!({
        "input_tokens": token("prompt_tokens")?,
        "output_tokens": token("completion_tokens")?,
        "cache_creation_input_tokens": Value::Null,
        "cache_read_input_tokens": Value::Null,
        "output_tokens_details": thinking,
        "server_tool_use": Value::Null,
        "cache_creation": Value::Null,
        "inference_geo": Value::Null,
        "service_tier": Value::Null,
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
    fn filters_prefill_and_maps_tool_controls() {
        let body = json!({
            "model": "gpt-4",
            "max_tokens": 10,
            "messages": [
                {"role": "user", "content": "Return JSON"},
                {"role": "assistant", "content": "{"}
            ],
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
        assert_eq!(
            converted["messages"],
            json!([{"role": "user", "content": "Return JSON"}])
        );
        assert!(converted["tools"][0]["function"].get("strict").is_none());
        assert_eq!(converted["tool_choice"]["function"]["name"], "lookup");
        assert!(converted.get("parallel_tool_calls").is_none());

        let unicode = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [{"role": "assistant", "content": "🙂🙂"}]
        });
        let converted = convert_request(&unicode, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][0]["content"], "🙂🙂");
    }

    #[test]
    fn preserves_reasoning_only_for_glm_and_qwen_tool_history() {
        for model in ["glm-5.2", "qwen3-coder"] {
            let body = json!({
                "model": model, "max_tokens": 10,
                "messages": [
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "private", "signature": "sig"},
                        {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "done"}
                    ]}
                ]
            });
            let converted = convert_request(&body, "https://example.com/v1").unwrap();
            assert_eq!(converted["messages"][0]["reasoning_content"], "private");
        }

        let generic = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "private", "signature": "sig"},
                    {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "done"}
                ]}
            ]
        });
        let converted = convert_request(&generic, "https://example.com/v1").unwrap();
        assert!(converted["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn validates_but_omits_structured_output_with_assistant_prefill() {
        let body = json!({
            "model": "gpt-5", "max_tokens": 10,
            "messages": [
                {"role": "user", "content": "Return JSON"},
                {"role": "assistant", "content": [{"type": "text", "text": "{"}]}
            ],
            "output_config": {
                "format": {"type": "json_schema", "schema": {"type": "object"}}
            }
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(
            converted["messages"],
            json!([{"role": "user", "content": "Return JSON"}])
        );
        assert!(converted.get("response_format").is_none());

        let only_prefill = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [{"role": "assistant", "content": "```"}]
        });
        assert!(convert_request(&only_prefill, "https://api.openai.com/v1").is_err());
    }

    #[test]
    fn ignores_empty_system_text_like_typescript() {
        let body = json!({
            "model": "gpt-4", "max_tokens": 10,
            "system": [{"type": "text", "text": ""}],
            "messages": [{"role": "user", "content": "hello"}]
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(
            converted["messages"],
            json!([{"role": "user", "content": "hello"}])
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
                {"role": "user", "content": null},
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
    fn repairs_duplicate_tool_ids_using_javascript_utf16_length() {
        let original = "abcdefg😀wxyz";
        let mut ids = IdContext::default();
        assert_eq!(unique_tool_id(original, &mut ids), original);
        let repaired = unique_tool_id(original, &mut ids);
        assert!(repaired.starts_with("abcdefg"));
        assert_eq!(
            repaired.encode_utf16().count(),
            original.encode_utf16().count()
        );
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

        let tool_image = json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": [{"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}]
            }]}]
        });
        assert!(convert_request(&tool_image, "https://api.openai.com/v1").is_err());
    }

    #[test]
    fn matches_main_typescript_request_behavior() {
        let body = json!({
            "model": "gpt-4",
            "max_tokens": 10,
            "stream": false,
            "system": "base",
            "top_k": 40,
            "messages": [
                {"role": "user", "content": "hello", "unknown": true},
                {"role": "system", "content": "mid-turn instruction"},
                {"role": "hook", "content": "hook text"},
                {"role": "user", "content": [{"type": "search_result", "content": []}]}
            ],
            "stop_sequences": [],
            "tool_choice": {"type": "none"},
            "output_config": {"format": {"type": "json_schema", "schema": {"type": "object"}}}
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(
            converted,
            json!({
                "model": "gpt-4",
                "messages": [
                    {"role": "system", "content": "base"},
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "mid-turn instruction"},
                    {"role": "assistant", "content": "hook text"}
                ],
                "stream": false,
                "max_tokens": 10,
                "stop": [],
                "tool_choice": "auto"
            })
        );
    }

    #[test]
    fn matches_main_tool_result_and_thinking_behavior() {
        let body = json!({
            "model": "gpt-4",
            "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [{"type": "thinking", "thinking": "private", "signature": "sig"}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "unmatched", "content": [
                        {"type": "text", "text": "one"},
                        {"type": "search_result", "content": []},
                        {"type": "text", "text": "two"}
                    ], "is_error": true},
                    {"type": "text", "text": "continue"}
                ]}
            ]
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(
            converted["messages"][0],
            json!({"role": "assistant", "content": null})
        );
        assert_eq!(
            converted["messages"][1],
            json!({"role": "tool", "tool_call_id": "unmatched", "content": "Error: one\ntwo"})
        );
        assert_eq!(
            converted["messages"][2],
            json!({"role": "user", "content": "continue"})
        );
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
            let mut response = json!({
                "id": "chatcmpl_1",
                "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": finish}],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 7,
                    "completion_tokens_details": {"reasoning_tokens": 3}
                }
            });
            if finish == "tool_calls" {
                response["choices"][0]["message"] = json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "toolu_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"}
                    }]
                });
            }
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
                "message": {"role": "assistant", "content": "partial", "refusal": "Cannot comply."},
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
            "choices": [{"message": {"role": "assistant", "content": null}, "finish_reason": "content_filter"}]
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
                "choices": [{"message": {"role": "assistant", "reasoning_content": "private"}, "finish_reason": "stop"}]
            }),
            json!({
                "id": "chatcmpl_2",
                "choices": [{"message": {"role": "assistant", "tool_calls": [{
                    "id": "toolu_1", "type": "function", "function": {"name": "lookup", "arguments": "not json"}
                }]}, "finish_reason": "tool_calls"}]
            }),
            json!({
                "id": "chatcmpl_3",
                "choices": [{"message": {"role": "assistant", "tool_calls": [{
                    "id": "toolu_1", "type": "function", "function": {"name": "lookup", "arguments": "[]"}
                }]}, "finish_reason": "tool_calls"}]
            }),
            json!({
                "id": "chatcmpl_4",
                "choices": [{"message": {"role": "assistant", "tool_calls": [{
                    "id": "toolu_1", "type": "function", "function": {"arguments": "{}"}
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
                "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": reason}]
            });
            assert!(convert_response(&response, "model").is_err());
        }
    }

    #[test]
    fn distinguishes_missing_usage_and_repairs_missing_response_ids() {
        let response = json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
        });
        let first = convert_response(&response, "model").unwrap();
        let second = convert_response(&response, "model").unwrap();
        assert!(first["id"].as_str().unwrap().starts_with("msg_"));
        assert_ne!(first["id"], second["id"]);
        assert_eq!(first["usage"]["input_tokens"], Value::Null);
        assert_eq!(first["usage"]["output_tokens"], Value::Null);

        let zero = json!({
            "id": "chatcmpl_1",
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0}
        });
        let zero = convert_response(&zero, "model").unwrap();
        assert_eq!(zero["usage"]["input_tokens"], 0);
        assert_eq!(zero["usage"]["output_tokens"], 0);
    }

    #[test]
    fn rejects_malformed_upstream_response_fields() {
        for response in [
            json!({"choices": [{"message": {"role": "user", "content": "ok"}, "finish_reason": "stop"}]}),
            json!({"choices": [{"message": {"role": "assistant", "content": "ok", "tool_calls": [{"type": "custom"}]}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}], "usage": "bad"}),
            json!({"choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": "1"}}),
            json!({"choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [{"message": {"role": "assistant", "tool_calls": [{"id": "toolu_1", "type": "function", "function": {"name": "lookup", "arguments": "{}"}}]}, "finish_reason": "stop"}]}),
        ] {
            let error = convert_response(&response, "model").unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        }
    }
}

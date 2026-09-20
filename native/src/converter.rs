use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::Rng;
use serde_json::{Map, Value, json};
use url::Url;

use crate::error::AppError;

const BILLING_HEADER: &str = "x-anthropic-billing-header:";
const IMAGE_MEDIA_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
const ASSISTANT_PREFILL_TOKENS: &[&str] = &["{", "[", "```", "{\"", "[{"];
const REQUEST_FIELDS: &[&str] = &[
    "model",
    "max_tokens",
    "messages",
    "system",
    "temperature",
    "top_p",
    "stream",
    "stop_sequences",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
    "cache_control",
    "metadata",
];

pub fn validate_request(body: &Value) -> Result<(), AppError> {
    let object = body
        .as_object()
        .ok_or_else(|| AppError::bad_request("body: Request body must be an object"))?;
    let mut errors = Vec::new();

    reject_unknown_fields(object, REQUEST_FIELDS, "", &mut errors);

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
    validate_stop_sequences(object.get("stop_sequences"), &mut errors);
    validate_cache_control(object.get("cache_control"), "cache_control", &mut errors);
    validate_metadata(object.get("metadata"), &mut errors);
    validate_output_config(object.get("output_config"), &mut errors);
    validate_thinking(
        object.get("thinking"),
        object.get("model").and_then(Value::as_str),
        &mut errors,
    );
    validate_tools_request(object.get("tools"), &mut errors);
    validate_tool_choice_request(object.get("tool_choice"), object.get("tools"), &mut errors);
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        validate_tool_history(messages, &mut errors);
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
        reject_unknown_fields(
            message,
            &["role", "content"],
            &format!("messages[{message_index}]"),
            errors,
        );
        let role = message.get("role").and_then(Value::as_str);
        match role {
            Some("user" | "assistant" | "system") => {}
            Some(_) => errors.push(format!(
                "messages[{message_index}].role: role must be user, assistant, or system"
            )),
            None => errors.push(format!(
                "messages[{message_index}].role: role is required and must be a string"
            )),
        }
        let contentless = message_object_is_contentless(message);
        if role == Some("system") && !contentless {
            validate_system_message(messages, message_index, message, errors);
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
    match (role, kind) {
        (Some("user"), "text") | (Some("system"), "text") => {
            validate_text_block(block, field, errors);
        }
        (Some("user"), "image") => validate_image_block(block, field, errors),
        (Some("user"), "tool_result") => validate_tool_result(block, field, errors),
        (Some("assistant"), "text") => validate_text_block(block, field, errors),
        (Some("assistant"), "tool_use") => validate_tool_use(block, field, errors),
        (Some("assistant"), "thinking") => {
            reject_unknown_fields(block, &["type", "thinking", "signature"], field, errors);
            for key in ["thinking", "signature"] {
                if block
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    errors.push(format!("{field}.{key}: {key} must be a non-empty string"));
                }
            }
        }
        (Some("assistant"), "redacted_thinking") => {
            reject_unknown_fields(block, &["type", "data"], field, errors);
            if block
                .get("data")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                errors.push(format!("{field}.data: data must be a non-empty string"));
            }
        }
        _ => {}
    }
}

fn validate_text_block(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    reject_unknown_fields(
        block,
        &["type", "text", "cache_control", "citations"],
        field,
        errors,
    );
    if block.get("text").and_then(Value::as_str).is_none() {
        errors.push(format!(
            "{field}.text: text is required and must be a string"
        ));
    }
    validate_cache_control(
        block.get("cache_control"),
        &format!("{field}.cache_control"),
        errors,
    );
    if block.get("citations").is_some_and(|value| !value.is_null()) {
        errors.push(format!("{field}.citations: citations are unsupported"));
    }
}

fn validate_image_block(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    reject_unknown_fields(
        block,
        &["type", "source", "cache_control", "transformations"],
        field,
        errors,
    );
    validate_cache_control(
        block.get("cache_control"),
        &format!("{field}.cache_control"),
        errors,
    );
    if block
        .get("transformations")
        .is_some_and(|value| !value.is_null())
    {
        errors.push(format!(
            "{field}.transformations: image transformations are unsupported"
        ));
    }
}

fn validate_tool_result(block: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    reject_unknown_fields(
        block,
        &[
            "type",
            "tool_use_id",
            "content",
            "is_error",
            "cache_control",
        ],
        field,
        errors,
    );
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
    validate_cache_control(
        block.get("cache_control"),
        &format!("{field}.cache_control"),
        errors,
    );
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
    reject_unknown_fields(
        block,
        &["type", "id", "name", "input", "cache_control"],
        field,
        errors,
    );
    if block
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(|name| !is_valid_tool_name(name))
    {
        errors.push(format!(
            "{field}.name: name must match [A-Za-z0-9_-]{{1,64}}"
        ));
    }
    if block.get("input").is_none_or(|input| !input.is_object()) {
        errors.push(format!("{field}.input: input must be an object"));
    }
    validate_cache_control(
        block.get("cache_control"),
        &format!("{field}.cache_control"),
        errors,
    );
}

fn validate_system_message(
    messages: &[Value],
    message_index: usize,
    message: &Map<String, Value>,
    errors: &mut Vec<String>,
) {
    let previous_role = messages[..message_index]
        .iter()
        .rev()
        .find(|message| !message_is_contentless(message))
        .and_then(message_role);
    if !matches!(previous_role, Some("user" | "system")) {
        errors.push(format!(
            "messages[{message_index}].role: system message must follow a user message"
        ));
    }
    let next_role = messages[message_index + 1..]
        .iter()
        .find(|message| !message_is_contentless(message))
        .and_then(message_role);
    if next_role.is_some() && !matches!(next_role, Some("assistant" | "system")) {
        errors.push(format!(
            "messages[{message_index}].role: system message must be last or followed by an assistant message"
        ));
    }
    for field in ["clear_at", "output_config"] {
        if message.contains_key(field) {
            errors.push(format!(
                "messages[{message_index}].{field}: {field} is not supported by this adapter"
            ));
        }
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
                if block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(str::is_empty)
                {
                    errors.push(format!("{field}.text: system text must not be empty"));
                }
            }
        }
        _ => errors.push("system: system must be a string or array of text blocks".to_owned()),
    }
}

fn validate_stop_sequences(stop: Option<&Value>, errors: &mut Vec<String>) {
    let Some(stop) = stop else {
        return;
    };
    let Some(sequences) = stop.as_array() else {
        errors.push("stop_sequences: stop_sequences must be an array".to_owned());
        return;
    };
    if sequences.len() > 4 {
        errors.push(
            "stop_sequences: OpenAI Chat Completions supports at most 4 stop sequences".to_owned(),
        );
    }
    for (index, sequence) in sequences.iter().enumerate() {
        if sequence.as_str().is_none_or(str::is_empty) {
            errors.push(format!(
                "stop_sequences[{index}]: stop sequence must be a non-empty string"
            ));
        }
    }
}

fn validate_cache_control(value: Option<&Value>, field: &str, errors: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    if value.is_null() {
        return;
    }
    let Some(cache) = value.as_object() else {
        errors.push(format!("{field}: cache_control must be an object or null"));
        return;
    };
    reject_unknown_fields(cache, &["type", "ttl"], field, errors);
    if cache.get("type").and_then(Value::as_str) != Some("ephemeral") {
        errors.push(format!(
            "{field}.type: cache_control type must be ephemeral"
        ));
    }
    if let Some(ttl) = cache.get("ttl")
        && !matches!(ttl.as_str(), Some("5m" | "1h"))
    {
        errors.push(format!("{field}.ttl: cache_control ttl must be 5m or 1h"));
    }
}

fn validate_metadata(value: Option<&Value>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    let Some(metadata) = value.as_object() else {
        errors.push("metadata: metadata must be an object".to_owned());
        return;
    };
    reject_unknown_fields(metadata, &["user_id"], "metadata", errors);
    if let Some(user_id) = metadata.get("user_id")
        && !user_id.is_null()
        && user_id.as_str().is_none_or(str::is_empty)
    {
        errors.push("metadata.user_id: user_id must be a non-empty string or null".to_owned());
    }
}

fn validate_output_config(value: Option<&Value>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    let Some(config) = value.as_object() else {
        errors.push("output_config: output_config must be an object".to_owned());
        return;
    };
    reject_unknown_fields(config, &["effort", "format"], "output_config", errors);
    if let Some(effort) = config.get("effort")
        && !effort.is_null()
        && !matches!(effort.as_str(), Some("low" | "medium" | "high" | "max"))
    {
        errors.push("output_config.effort: effort must be low, medium, high, or max".to_owned());
    }
    if let Some(format) = config.get("format")
        && !format.is_null()
    {
        let Some(format) = format.as_object() else {
            errors.push("output_config.format: format must be an object or null".to_owned());
            return;
        };
        reject_unknown_fields(format, &["type", "schema"], "output_config.format", errors);
        if format.get("type").and_then(Value::as_str) != Some("json_schema") {
            errors.push("output_config.format.type: format type must be json_schema".to_owned());
        }
        if format
            .get("schema")
            .is_none_or(|schema| !schema.is_object())
        {
            errors.push("output_config.format.schema: schema must be an object".to_owned());
        }
    }
}

fn validate_thinking(value: Option<&Value>, model: Option<&str>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    let Some(thinking) = value.as_object() else {
        errors.push("thinking: thinking must be an object".to_owned());
        return;
    };
    reject_unknown_fields(thinking, &["type", "budget_tokens"], "thinking", errors);
    match thinking.get("type").and_then(Value::as_str) {
        Some("disabled") => {
            if thinking.contains_key("budget_tokens") {
                errors.push(
                    "thinking.budget_tokens: budget_tokens is not valid when thinking is disabled"
                        .to_owned(),
                );
            }
        }
        Some("enabled" | "adaptive")
            if model.is_some_and(|model| is_glm5(model) || is_qwen3(model)) => {}
        Some("enabled" | "adaptive") => errors.push(
            "thinking.type: enabled and adaptive thinking have no exact OpenAI Chat Completions mapping; use output_config.effort"
                .to_owned(),
        ),
        Some(_) => errors.push(
            "thinking.type: thinking type must be enabled, disabled, or adaptive".to_owned(),
        ),
        None => errors.push("thinking.type: thinking type is required".to_owned()),
    }
}

fn validate_tools_request(value: Option<&Value>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    let Some(tools) = value.as_array() else {
        errors.push("tools: tools must be an array".to_owned());
        return;
    };
    let mut names = HashSet::new();
    for (index, tool) in tools.iter().enumerate() {
        let field = format!("tools[{index}]");
        let Some(tool) = tool.as_object() else {
            errors.push(format!("{field}: tool must be an object"));
            continue;
        };
        reject_unknown_fields(
            tool,
            &[
                "type",
                "name",
                "description",
                "input_schema",
                "strict",
                "cache_control",
                "allowed_callers",
                "defer_loading",
            ],
            &field,
            errors,
        );
        match tool.get("type") {
            None => {}
            Some(Value::String(kind)) if kind == "custom" => {}
            Some(Value::String(kind)) => errors.push(format!(
                "{field}.type: Anthropic server tool {kind} is unsupported"
            )),
            Some(_) => errors.push(format!(
                "{field}.type: tool type must be custom when provided"
            )),
        }
        match tool.get("name").and_then(Value::as_str) {
            Some(name) if is_valid_tool_name(name) => {
                if !names.insert(name) {
                    errors.push(format!("{field}.name: duplicate tool name {name}"));
                }
            }
            _ => errors.push(format!(
                "{field}.name: name must match [A-Za-z0-9_-]{{1,64}}"
            )),
        }
        if tool
            .get("input_schema")
            .is_none_or(|schema| !schema.is_object())
        {
            errors.push(format!(
                "{field}.input_schema: input_schema must be an object"
            ));
        }
        if tool
            .get("description")
            .is_some_and(|value| !value.is_string())
        {
            errors.push(format!("{field}.description: description must be a string"));
        }
        if tool.get("strict").is_some_and(|value| !value.is_boolean()) {
            errors.push(format!("{field}.strict: strict must be a boolean"));
        }
        validate_cache_control(
            tool.get("cache_control"),
            &format!("{field}.cache_control"),
            errors,
        );
        if tool.contains_key("allowed_callers") || tool.contains_key("defer_loading") {
            errors.push(format!(
                "{field}: allowed_callers and defer_loading are unsupported"
            ));
        }
    }
}

fn validate_tool_choice_request(
    value: Option<&Value>,
    tools: Option<&Value>,
    errors: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    let Some(choice) = value.as_object() else {
        errors.push("tool_choice: tool_choice must be an object".to_owned());
        return;
    };
    reject_unknown_fields(
        choice,
        &["type", "name", "disable_parallel_tool_use"],
        "tool_choice",
        errors,
    );
    if choice
        .get("disable_parallel_tool_use")
        .is_some_and(|value| !value.is_boolean())
    {
        errors.push(
            "tool_choice.disable_parallel_tool_use: disable_parallel_tool_use must be a boolean"
                .to_owned(),
        );
    }
    let declared_tools: HashSet<&str> = tools
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    match choice.get("type").and_then(Value::as_str) {
        Some("tool") => match choice.get("name").and_then(Value::as_str) {
            Some(name) if !is_valid_tool_name(name) => {
                errors.push("tool_choice.name: name must match [A-Za-z0-9_-]{1,64}".to_owned())
            }
            Some(name) if !declared_tools.contains(name) => errors.push(format!(
                "tool_choice.name: named tool choice references undeclared tool {name}"
            )),
            Some(_) => {}
            None => errors
                .push("tool_choice.name: named tool choice requires a non-empty name".to_owned()),
        },
        Some("any") => {
            if choice.contains_key("name") {
                errors.push("tool_choice.name: name is only valid for type tool".to_owned());
            }
            if declared_tools.is_empty() {
                errors.push("tool_choice.type: any requires at least one tool".to_owned());
            }
        }
        Some("none" | "auto") => {
            if choice.contains_key("name") {
                errors.push("tool_choice.name: name is only valid for type tool".to_owned());
            }
            if choice.get("type").and_then(Value::as_str) == Some("none")
                && choice.contains_key("disable_parallel_tool_use")
            {
                errors.push(
                    "tool_choice.disable_parallel_tool_use: field is not valid for type none"
                        .to_owned(),
                );
            }
        }
        Some(_) => errors
            .push("tool_choice.type: tool choice type must be none, auto, any, or tool".to_owned()),
        None => errors.push("tool_choice.type: tool choice type is required".to_owned()),
    }
}

fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_tool_history(messages: &[Value], errors: &mut Vec<String>) {
    let mut pending: Option<(usize, Vec<String>)> = None;
    for (message_index, message) in messages.iter().enumerate() {
        let Some(message) = message.as_object() else {
            continue;
        };
        if message_object_is_contentless(message) {
            continue;
        }
        let role = message.get("role").and_then(Value::as_str);
        let blocks = message.get("content").and_then(Value::as_array);
        let result_ids: Vec<String> = blocks
            .into_iter()
            .flatten()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|block| {
                block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();

        if let Some((assistant_index, expected_ids)) = pending.take() {
            if role != Some("user") {
                errors.push(format!(
                    "messages[{message_index}]: tool results must immediately follow messages[{assistant_index}]"
                ));
            } else {
                let mut expected = HashMap::new();
                let mut actual = HashMap::new();
                for id in expected_ids {
                    *expected.entry(id).or_insert(0usize) += 1;
                }
                for id in &result_ids {
                    *actual.entry(id.clone()).or_insert(0usize) += 1;
                }
                if expected != actual {
                    errors.push(format!(
                        "messages[{message_index}]: tool_result IDs must exactly match tool_use IDs from messages[{assistant_index}]"
                    ));
                }
                if let Some(blocks) = blocks {
                    let mut saw_non_result = false;
                    for (block_index, block) in blocks.iter().enumerate() {
                        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                            if saw_non_result {
                                errors.push(format!(
                                    "messages[{message_index}].content[{block_index}]: tool_result blocks must precede ordinary user content"
                                ));
                            }
                        } else {
                            saw_non_result = true;
                        }
                    }
                }
            }
        } else if !result_ids.is_empty() {
            errors.push(format!(
                "messages[{message_index}]: tool_result has no immediately preceding tool_use turn"
            ));
        }

        if role == Some("assistant") {
            let tool_ids: Vec<String> = blocks
                .into_iter()
                .flatten()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .map(|block| {
                    block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            if !tool_ids.is_empty() {
                pending = Some((message_index, tool_ids));
            }
        }
    }
    if let Some((assistant_index, _)) = pending {
        errors.push(format!(
            "messages[{assistant_index}]: tool_use turn is missing its following tool_result message"
        ));
    }
}

fn message_is_contentless(message: &Value) -> bool {
    message
        .as_object()
        .is_some_and(message_object_is_contentless)
}

fn message_role(message: &Value) -> Option<&str> {
    message
        .as_object()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
}

fn message_object_is_contentless(message: &Map<String, Value>) -> bool {
    message.get("content").is_none_or(Value::is_null)
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    prefix: &str,
    errors: &mut Vec<String>,
) {
    for key in object.keys().filter(|key| !allowed.contains(&key.as_str())) {
        let field = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        errors.push(format!("{field}: unsupported field"));
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
    let mut system_parts = Vec::new();

    if let Some(system) = request.get("system") {
        let content = system_content(system);
        let content = strip_billing_header(&content);
        if !content.is_empty() {
            system_parts.push(content);
        }
    }

    let mut ids = IdContext::default();
    let mut stripped_thinking = false;
    let preserve_reasoning = is_glm5(model) || is_qwen3(model);
    for (message_index, message) in request["messages"]
        .as_array()
        .expect("validated messages")
        .iter()
        .enumerate()
    {
        if message_is_contentless(message) {
            continue;
        }
        if message.get("role").and_then(Value::as_str) == Some("system") {
            let content = message_content_text(message);
            if !content.is_empty() {
                system_parts.push(content);
            }
            continue;
        }
        messages.extend(convert_message(
            message,
            message_index,
            &mut ids,
            &mut stripped_thinking,
            preserve_reasoning,
        )?);
    }
    if !system_parts.is_empty() {
        messages.insert(
            0,
            json!({"role": "system", "content": system_parts.join("\n\n")}),
        );
    }
    if messages.is_empty() {
        return Err(AppError::bad_request(
            "No messages after conversion: all input messages had missing content",
        ));
    }
    if stripped_thinking {
        eprintln!("[adapter] Stripped unsupported Anthropic thinking history from request");
    }
    let mut omitted_fields = Vec::new();
    if request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("strict").and_then(Value::as_bool) == Some(true))
        })
    {
        omitted_fields.push("tools[].strict");
    }
    if request
        .get("tool_choice")
        .and_then(|choice| choice.get("disable_parallel_tool_use"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        omitted_fields.push("tool_choice.disable_parallel_tool_use");
    }
    if request
        .get("output_config")
        .and_then(|config| config.get("format"))
        .is_some_and(|format| !format.is_null())
    {
        omitted_fields.push("output_config.format");
    }
    if !omitted_fields.is_empty() {
        eprintln!(
            "[adapter] Validated input fields not sent upstream: {}",
            omitted_fields.join(", ")
        );
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
    if let Some(stop) = request
        .get("stop_sequences")
        .and_then(Value::as_array)
        .filter(|stop| !stop.is_empty())
    {
        if does_not_support_stop(model) {
            return Err(AppError::bad_request(format!(
                "stop_sequences: {model} does not support stop sequences"
            )));
        }
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
        let choice_type = choice.get("type").and_then(Value::as_str);
        if tools.is_some() || !matches!(choice_type, Some("none" | "auto")) {
            output.insert("tool_choice".into(), convert_tool_choice(choice)?);
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
        _ => unreachable!("validated system content"),
    }
}

fn message_content_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(content) => content.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => unreachable!("validated system message content"),
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
    preserve_reasoning: bool,
) -> Result<Vec<Value>, AppError> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let Some(content) = message.get("content").filter(|content| !content.is_null()) else {
        return Ok(Vec::new());
    };
    if let Some(content) = content.as_str() {
        if role == "assistant" && is_assistant_prefill(content) {
            return Ok(Vec::new());
        }
        return Ok(vec![json!({"role": role, "content": content})]);
    }

    let blocks = content.as_array().expect("validated content array");
    match role {
        "user" => convert_user_blocks(blocks, message_index, ids),
        _ => {
            let converted = convert_assistant_blocks(
                blocks,
                message_index,
                ids,
                stripped_thinking,
                preserve_reasoning,
            )?;
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
    let mut tool_image_parts = Vec::new();

    for (block_index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => user_parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).expect("validated text")
            })),
            Some("image") => user_parts.push(convert_image(block)?),
            Some("tool_result") => {
                let original_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .expect("validated tool_use_id");
                let resolved_id = resolve_tool_result_id(original_id, ids).ok_or_else(|| {
                    AppError::bad_request(format!(
                        "messages[{message_index}].content[{block_index}].tool_use_id: no unmatched tool_use for {original_id}"
                    ))
                })?;
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
    if !tool_image_parts.is_empty() {
        result.push(json!({"role": "user", "content": tool_image_parts}));
    }
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
                            .expect("validated text")
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
    let allowed_source_fields: &[&str] = match source_type {
        "base64" => &["type", "media_type", "data"],
        "url" => &["type", "url"],
        "file" | "file_id" => &["type", "file_id"],
        _ => &["type"],
    };
    if let Some(field) = source
        .keys()
        .find(|key| !allowed_source_fields.contains(&key.as_str()))
    {
        return Err(AppError::bad_request(format!(
            "image.source.{field}: unsupported field"
        )));
    }
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
    preserve_reasoning: bool,
) -> Result<Value, AppError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut saw_redacted_thinking = false;
    let mut tool_calls = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
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
            Some("redacted_thinking") => saw_redacted_thinking = true,
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
        if preserve_reasoning && !reasoning.is_empty() {
            message.insert("reasoning_content".into(), Value::String(reasoning));
        } else if !reasoning.is_empty() {
            *stripped_thinking = true;
        }
    } else if !reasoning.is_empty() {
        *stripped_thinking = true;
    }
    if saw_redacted_thinking {
        *stripped_thinking = true;
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

fn resolve_tool_result_id(original: &str, ids: &mut IdContext) -> Option<String> {
    let mappings = ids.mappings.get(original)?;
    let index = ids.result_index.entry(original.to_owned()).or_default();
    let resolved = mappings.get(*index).cloned()?;
    *index += 1;
    Some(resolved)
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
        && let Some(effort) = effort
    {
        output.insert(
            "reasoning_effort".into(),
            Value::String(if effort == "max" { "xhigh" } else { effort }.to_owned()),
        );
    }
    Ok(())
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
fn does_not_support_stop(model: &str) -> bool {
    let model = normalized(model);
    model == "o3" || model.starts_with("o3-") || model == "o4-mini" || model.starts_with("o4-mini-")
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
            "messages": [{"role": "tool", "content": "hidden"}]
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
    fn validates_mid_conversation_system_message_placement() {
        let valid = json!({
            "model": "gpt-5.2", "max_tokens": 10,
            "system": "base",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "system", "content": "be concise"},
                {"role": "system", "content": [{"type": "text", "text": "use JSON"}]}
            ]
        });
        validate_request(&valid).unwrap();
        let converted = convert_request(&valid, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["messages"][0]["role"], "system");
        assert_eq!(
            converted["messages"][0]["content"],
            "base\n\nbe concise\n\nuse JSON"
        );
        assert_eq!(converted["messages"][1]["role"], "user");

        let with_contentless_hook = json!({
            "model": "gpt-5.2", "max_tokens": 10,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "system", "content": "be concise"},
                {"role": "user", "content": null},
                {"role": "assistant", "content": "understood"}
            ]
        });
        validate_request(&with_contentless_hook).unwrap();

        for body in [
            json!({
                "model": "gpt-5.2", "max_tokens": 10,
                "messages": [{"role": "system", "content": "first"}]
            }),
            json!({
                "model": "gpt-5.2", "max_tokens": 10,
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "hi"},
                    {"role": "system", "content": "misplaced"}
                ]
            }),
            json!({
                "model": "gpt-5.2", "max_tokens": 10,
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "system", "content": "misplaced"},
                    {"role": "user", "content": "continue"}
                ]
            }),
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
    fn validates_strict_request_fields_and_maps_exact_options() {
        for body in [
            json!({"model": "gpt-5", "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}], "top_k": 1}),
            json!({"model": "gpt-5", "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}], "tool_choice": {"type": "auto", "name": "lookup"}}),
            json!({"model": "gpt-5", "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}], "cache_control": {"type": "ephemeral", "ttl": "2h"}}),
            json!({"model": "gpt-5", "max_tokens": 1, "messages": [{"role": "user", "content": "ok"}], "thinking": {"type": "enabled", "budget_tokens": 1024}}),
        ] {
            assert!(
                validate_request(&body).is_err(),
                "body should be rejected: {body}"
            );
        }

        let contentless_hooks = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [
                {"role": "system"},
                {"role": "user"},
                {"role": "assistant", "content": null},
                {"role": "user", "content": "continue"}
            ]
        });
        let converted = convert_request(&contentless_hooks, "https://example.com/v1").unwrap();
        assert_eq!(
            converted["messages"],
            json!([{"role": "user", "content": "continue"}])
        );

        let contentless_system_hook = json!({
            "model": "gpt-4", "max_tokens": 10,
            "system": "Base instruction",
            "messages": [
                {"role": "user", "content": "continue"},
                {"role": "system"}
            ]
        });
        let converted =
            convert_request(&contentless_system_hook, "https://example.com/v1").unwrap();
        assert_eq!(
            converted["messages"],
            json!([
                {"role": "system", "content": "Base instruction"},
                {"role": "user", "content": "continue"}
            ])
        );

        let system_only_after_hooks = json!({
            "model": "gpt-4", "max_tokens": 10,
            "system": "Base instruction",
            "messages": [{"role": "user"}]
        });
        let converted =
            convert_request(&system_only_after_hooks, "https://example.com/v1").unwrap();
        assert_eq!(
            converted["messages"],
            json!([{"role": "system", "content": "Base instruction"}])
        );

        let body = json!({
            "model": "gpt-5",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ok"}],
            "metadata": {"user_id": "user_1"},
            "cache_control": {"type": "ephemeral", "ttl": "1h"},
            "output_config": {
                "effort": "high",
                "format": {"type": "json_schema", "schema": {"type": "object"}}
            }
        });
        let converted = convert_request(&body, "https://api.openai.com/v1").unwrap();
        assert_eq!(converted["max_completion_tokens"], 1);
        assert_eq!(converted["reasoning_effort"], "high");
        assert!(converted.get("safety_identifier").is_none());
        assert!(converted.get("response_format").is_none());
        assert!(converted.get("cache_control").is_none());

        let disabled = json!({
            "model": "gpt-5", "max_tokens": 1,
            "messages": [{"role": "user", "content": "ok"}],
            "thinking": {"type": "disabled"}
        });
        assert!(
            convert_request(&disabled, "https://api.openai.com/v1")
                .unwrap()
                .get("reasoning_effort")
                .is_none()
        );

        let qwen = json!({
            "model": "qwen3-coder", "max_tokens": 1,
            "messages": [{"role": "user", "content": "ok"}],
            "thinking": {"type": "disabled"},
            "output_config": {"effort": "high"}
        });
        assert!(
            convert_request(&qwen, "https://api.openai.com/v1")
                .unwrap()
                .get("reasoning_effort")
                .is_none()
        );
    }

    #[test]
    fn emits_only_compatible_optional_fields() {
        let generic = json!({
            "model": "gpt-4", "max_tokens": 1,
            "messages": [{"role": "user", "content": "ok"}],
            "metadata": {"user_id": "user_1"},
            "stop_sequences": [],
            "output_config": {"effort": "high"},
            "tools": [{
                "name": "lookup", "input_schema": {"type": "object"}, "strict": false
            }]
        });
        let converted = convert_request(&generic, "https://example.com/v1").unwrap();
        assert_eq!(converted["max_tokens"], 1);
        for field in [
            "max_completion_tokens",
            "safety_identifier",
            "stop",
            "reasoning_effort",
            "tool_choice",
        ] {
            assert!(converted.get(field).is_none(), "unexpected field {field}");
        }
        assert!(converted["tools"][0]["function"].get("strict").is_none());

        let azure = convert_request(
            &json!({
                "model": "gpt-4", "max_tokens": 1,
                "messages": [{"role": "user", "content": "ok"}]
            }),
            "https://example.openai.azure.com/openai/deployments/test",
        )
        .unwrap();
        assert_eq!(azure["max_tokens"], 32);

        let unsupported_stop = json!({
            "model": "o4-mini", "max_tokens": 10,
            "messages": [{"role": "user", "content": "ok"}],
            "stop_sequences": ["done"]
        });
        assert!(
            convert_request(&unsupported_stop, "https://api.openai.com/v1")
                .unwrap_err()
                .message
                .contains("does not support")
        );
    }

    #[test]
    fn validates_tool_names_and_choices() {
        for body in [
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "ok"}],
                "tools": [
                    {"name": "same", "input_schema": {}},
                    {"name": "same", "input_schema": {}}
                ]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "ok"}],
                "tools": [{"name": "invalid name", "input_schema": {}}]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "invalid name", "input": {}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "done"}
                    ]}
                ]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "ok"}],
                "tools": [{"name": "lookup", "input_schema": {}}],
                "tool_choice": {"type": "tool", "name": "missing"}
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": "ok"}],
                "tool_choice": {"type": "any"}
            }),
        ] {
            assert!(validate_request(&body).is_err(), "body should fail: {body}");
        }
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
    fn validates_tool_results_and_keeps_images_before_user_text() {
        for body in [
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [{"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "missing", "content": "done"}
                ]}]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "text", "text": "before"},
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "done"}
                    ]}
                ]
            }),
            json!({
                "model": "gpt-4", "max_tokens": 10,
                "messages": [
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "name": "lookup", "input": {}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "done"}
                    ]}
                ]
            }),
        ] {
            assert!(validate_request(&body).is_err(), "body should fail: {body}");
        }

        let valid = json!({
            "model": "gpt-4o", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "shot", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}
                    ]},
                    {"type": "text", "text": "continue"}
                ]}
            ]
        });
        let converted = convert_request(&valid, "https://example.com/v1").unwrap();
        assert_eq!(converted["messages"][1]["role"], "tool");
        assert_eq!(converted["messages"][2]["content"][0]["type"], "text");
        assert_eq!(converted["messages"][2]["content"][1]["type"], "image_url");
        assert_eq!(converted["messages"][3]["content"], "continue");
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
            assert!(convert_request(&body, "https://api.openai.com/v1").is_err());
        }
    }

    #[test]
    fn strips_thinking_history_only_when_visible_content_remains() {
        let valid = json!({
            "model": "gpt-4", "max_tokens": 10,
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "private", "signature": "sig"},
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

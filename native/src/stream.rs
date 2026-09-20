use std::{collections::BTreeMap, convert::Infallible};

use async_stream::stream;
use axum::body::Body;
use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use rand::Rng;
use serde_json::{Value, json};

use crate::{converter::map_finish_reason, storage::Storage};

pub fn transform_stream(
    response: reqwest::Response,
    model: String,
    provider: String,
    storage: Storage,
    request_id: String,
) -> Body {
    let output = stream! {
        let mut upstream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut state = StreamState::new(model, provider, request_id);
        let mut failed = false;
        let mut done = false;

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    decoder.push(&bytes);
                    while let Some(data) = decoder.next_data() {
                        if data == "[DONE]" {
                            done = true;
                            break;
                        }
                        match serde_json::from_str::<Value>(&data) {
                            Ok(chunk) => match state.process_chunk(&chunk) {
                                Ok(events) => {
                                    for event in events {
                                        yield Ok::<Bytes, Infallible>(encode_event(event));
                                    }
                                }
                                Err(message) => {
                                    storage.record_error(502, state.error_record(&message));
                                    yield Ok(encode_event(error_event(&message)));
                                    failed = true;
                                    break;
                                }
                            }
                            Err(error) => {
                                let message = format!("Invalid upstream SSE JSON: {error}");
                                storage.record_error(502, state.error_record(&message));
                                yield Ok(encode_event(error_event(&message)));
                                failed = true;
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    let message = format!("Upstream stream failed: {error}");
                    storage.record_error(502, state.error_record(&message));
                    yield Ok(encode_event(error_event(&message)));
                    failed = true;
                }
            }
            if failed || done {
                break;
            }
        }

        if !failed {
            match state.finish() {
                Ok(events) => {
                    storage.record_usage(state.usage_record());
                    for event in events {
                        yield Ok(encode_event(event));
                    }
                }
                Err(message) => {
                    storage.record_error(502, state.error_record(&message));
                    yield Ok(encode_event(error_event(&message)));
                }
            }
        }
    };
    Body::from_stream(output)
}

fn encode_event(event: Value) -> Bytes {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Bytes::from(format!(
        "event: {event_type}\ndata: {}\n\n",
        serde_json::to_string(&event).expect("SSE event serialization")
    ))
}

fn error_event(message: &str) -> Value {
    json!({"type": "error", "error": {"type": "api_error", "message": message}})
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next_data(&mut self) -> Option<String> {
        loop {
            let (position, delimiter_length) = find_delimiter(&self.buffer)?;
            let event = self.buffer.drain(..position).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_length);
            let event = String::from_utf8_lossy(&event);
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>();
            if !data.is_empty() {
                return Some(data.join("\n"));
            }
        }
    }
}

fn find_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(position), None) => Some((position, 2)),
        (None, Some(position)) => Some((position, 4)),
        (None, None) => None,
    }
}

struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct StreamState {
    message_id: String,
    request_id: String,
    model: String,
    response_model: Option<String>,
    provider: String,
    content_index: usize,
    tools: BTreeMap<usize, ToolCall>,
    started: bool,
    text_open: bool,
    tools_closed: bool,
    reasoning_seen: bool,
    visible_content_seen: bool,
    refusal_text: String,
    deferred_text: String,
    finish_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    thinking_tokens: Option<u64>,
    upstream_usage: Option<Value>,
}

impl StreamState {
    fn new(model: String, provider: String, request_id: String) -> Self {
        Self {
            message_id: format!("msg_{}", Utc::now().timestamp_millis()),
            request_id,
            model,
            response_model: None,
            provider,
            content_index: 0,
            tools: BTreeMap::new(),
            started: false,
            text_open: false,
            tools_closed: false,
            reasoning_seen: false,
            visible_content_seen: false,
            refusal_text: String::new(),
            deferred_text: String::new(),
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
            thinking_tokens: None,
            upstream_usage: None,
        }
    }

    fn process_chunk(&mut self, chunk: &Value) -> Result<Vec<Value>, String> {
        let mut events = Vec::new();
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            let usage = usage
                .as_object()
                .ok_or_else(|| "Upstream stream usage must be an object or null".to_owned())?;
            for (field, target) in [
                ("prompt_tokens", &mut self.input_tokens),
                ("completion_tokens", &mut self.output_tokens),
            ] {
                if let Some(value) = usage.get(field)
                    && !value.is_null()
                {
                    *target = Some(value.as_u64().ok_or_else(|| {
                        format!("Upstream stream usage.{field} must be an integer")
                    })?);
                }
            }
            if let Some(details) = usage.get("completion_tokens_details")
                && !details.is_null()
                && !details.is_object()
            {
                return Err(
                    "Upstream stream usage.completion_tokens_details must be an object or null"
                        .to_owned(),
                );
            }
            if let Some(tokens) = usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                && !tokens.is_null()
            {
                self.thinking_tokens = Some(tokens.as_u64().ok_or_else(|| {
                    "Upstream stream usage.completion_tokens_details.reasoning_tokens must be an integer"
                        .to_owned()
                })?);
            }
            if !usage.is_empty() {
                self.upstream_usage = Some(Value::Object(usage.clone()));
            }
        }
        if self.response_model.is_none() {
            self.response_model = chunk
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }

        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| "Upstream stream chunk is missing choices".to_owned())?;
        if choices.is_empty() {
            if chunk.get("usage").is_some_and(is_non_empty_object) {
                return Ok(events);
            }
            return Err("Upstream stream chunk has no choice or usage".to_owned());
        }
        if choices.len() != 1 {
            return Err("Upstream stream chunk must contain exactly one choice".to_owned());
        }
        let choice = &choices[0];
        if !self.started {
            events.push(json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": Value::Null,
                    "stop_details": Value::Null,
                    "stop_sequence": Value::Null,
                    "container": Value::Null,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "cache_creation_input_tokens": Value::Null,
                        "cache_read_input_tokens": Value::Null,
                        "output_tokens_details": Value::Null,
                        "server_tool_use": Value::Null,
                        "cache_creation": Value::Null,
                        "inference_geo": Value::Null,
                        "service_tier": Value::Null
                    }
                }
            }));
            self.started = true;
        }
        let delta = choice
            .get("delta")
            .and_then(Value::as_object)
            .ok_or_else(|| "Upstream stream choice is missing delta".to_owned())?;
        if delta
            .get("role")
            .is_some_and(|role| !role.is_null() && role.as_str() != Some("assistant"))
        {
            return Err("Upstream stream delta.role must be assistant or null".to_owned());
        }
        for field in ["content", "refusal", "reasoning_content", "reasoning"] {
            if delta
                .get(field)
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err(format!(
                    "Upstream stream delta.{field} must be a string or null"
                ));
            }
        }
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !reasoning.is_empty() {
            self.reasoning_seen = true;
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.push_text(text, &mut events);
        }
        if let Some(refusal) = delta
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.refusal_text.push_str(refusal);
            self.push_text(refusal, &mut events);
        }
        if delta
            .get("tool_calls")
            .is_some_and(|calls| !calls.is_null() && !calls.is_array())
        {
            return Err("Upstream stream delta.tool_calls must be an array".to_owned());
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                self.process_tool_delta(tool_call, &mut events)?;
            }
        }
        if choice
            .get("finish_reason")
            .is_some_and(|reason| !reason.is_null())
        {
            self.close_text(&mut events);
            self.finish_reason = Some(
                choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "Upstream stream finish_reason must be a string or null".to_owned()
                    })?
                    .to_owned(),
            );
        }
        Ok(events)
    }

    fn process_tool_delta(&mut self, delta: &Value, events: &mut Vec<Value>) -> Result<(), String> {
        let delta = delta
            .as_object()
            .ok_or_else(|| "Upstream tool delta must be an object".to_owned())?;
        let index = delta
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "Upstream tool delta is missing index".to_owned())?
            as usize;
        if delta
            .get("type")
            .is_some_and(|kind| !kind.is_null() && kind.as_str() != Some("function"))
        {
            return Err("Upstream tool delta type must be function or null".to_owned());
        }
        if delta
            .get("id")
            .is_some_and(|id| !id.is_null() && !id.is_string())
        {
            return Err("Upstream tool delta id must be a string or null".to_owned());
        }
        if delta
            .get("function")
            .is_some_and(|function| !function.is_null() && !function.is_object())
        {
            return Err("Upstream tool delta function must be an object or null".to_owned());
        }
        let function = delta.get("function").and_then(Value::as_object);
        if !self.tools.contains_key(&index) {
            self.close_text(events);
            let id = delta
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !self.tools.values().any(|call| call.id == *id))
                .map(str::to_owned)
                .unwrap_or_else(generated_tool_id);
            self.tools.insert(
                index,
                ToolCall {
                    id: id.clone(),
                    name: String::new(),
                    arguments: String::new(),
                },
            );
        }
        if let Some(arguments) = function.and_then(|function| function.get("arguments"))
            && !arguments.is_null()
            && !arguments.is_string()
        {
            return Err("Upstream tool delta function.arguments must be a string".to_owned());
        }
        if let Some(name) = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            && !name.is_empty()
        {
            self.tools
                .get_mut(&index)
                .expect("inserted tool")
                .name
                .push_str(name);
        }
        if let Some(arguments) = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            let tool = self.tools.get_mut(&index).expect("inserted tool");
            tool.arguments.push_str(arguments);
        }
        self.visible_content_seen = true;
        Ok(())
    }

    fn push_text(&mut self, text: &str, events: &mut Vec<Value>) {
        if !self.tools.is_empty() && !self.tools_closed {
            self.deferred_text.push_str(text);
            self.visible_content_seen = true;
            return;
        }
        if !self.text_open {
            events.push(json!({
                "type": "content_block_start",
                "index": self.content_index,
                "content_block": {"type": "text", "text": ""}
            }));
            self.text_open = true;
        }
        events.push(json!({
            "type": "content_block_delta",
            "index": self.content_index,
            "delta": {"type": "text_delta", "text": text}
        }));
        self.visible_content_seen = true;
    }

    fn close_text(&mut self, events: &mut Vec<Value>) {
        if self.text_open {
            events.push(json!({"type": "content_block_stop", "index": self.content_index}));
            self.text_open = false;
            self.content_index += 1;
        }
    }

    fn emit_tools(&mut self, events: &mut Vec<Value>) {
        if !self.tools_closed {
            for tool in self.tools.values() {
                let index = self.content_index;
                self.content_index += 1;
                events.push(json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {"type": "tool_use", "id": tool.id, "name": tool.name, "input": {}}
                }));
                if !tool.arguments.is_empty() {
                    events.push(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": tool.arguments}
                    }));
                }
                events.push(json!({"type": "content_block_stop", "index": index}));
            }
            self.tools_closed = true;
        }
    }

    fn finish(&mut self) -> Result<Vec<Value>, String> {
        let mut events = Vec::new();
        let finish_reason = self
            .finish_reason
            .as_deref()
            .ok_or_else(|| "Upstream stream ended without a finish_reason".to_owned())?;
        let mapped_finish = map_finish_reason(finish_reason).map_err(|error| error.message)?;
        if (finish_reason == "tool_calls") != !self.tools.is_empty() {
            return Err("Upstream tool calls do not match finish_reason".to_owned());
        }
        for tool in self.tools.values() {
            if tool.name.is_empty() {
                return Err("Upstream tool call is missing function.name".to_owned());
            }
            let input: Value = serde_json::from_str(&tool.arguments)
                .map_err(|error| format!("Upstream tool arguments are invalid JSON: {error}"))?;
            if !input.is_object() {
                return Err("Upstream tool arguments must decode to a JSON object".to_owned());
            }
        }
        let filtered = finish_reason == "content_filter";
        let explanation = if self.refusal_text.is_empty() {
            "Response withheld by the upstream content filter.".to_owned()
        } else {
            self.refusal_text.clone()
        };
        if filtered && !self.visible_content_seen {
            self.push_text(&explanation, &mut events);
        }
        if self.reasoning_seen && !self.visible_content_seen {
            return Err(
                "Upstream stream contained reasoning but no user-visible content".to_owned(),
            );
        }
        self.close_text(&mut events);
        self.emit_tools(&mut events);
        if !self.deferred_text.is_empty() {
            let text = std::mem::take(&mut self.deferred_text);
            self.push_text(&text, &mut events);
            self.close_text(&mut events);
        }
        let is_refusal = filtered || !self.refusal_text.is_empty();
        let stop_reason = if is_refusal { "refusal" } else { mapped_finish };
        let stop_details = if is_refusal {
            json!({"type": "refusal", "category": Value::Null, "explanation": explanation})
        } else {
            Value::Null
        };
        let usage = json!({
            "input_tokens": self.input_tokens.map(Value::from),
            "output_tokens": self.output_tokens.map(Value::from),
            "cache_creation_input_tokens": Value::Null,
            "cache_read_input_tokens": Value::Null,
            "output_tokens_details": self.thinking_tokens.map(|tokens| json!({"thinking_tokens": tokens})),
            "server_tool_use": Value::Null
        });
        events.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null, "stop_details": stop_details},
            "usage": usage
        }));
        events.push(json!({"type": "message_stop"}));
        Ok(events)
    }

    fn usage_record(&self) -> Value {
        let mut record = json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "schemaVersion": 2,
            "provider": self.provider,
            "modelName": self.model,
            "streaming": true,
            "usageStatus": if self.upstream_usage.is_some() { "complete" } else { "missing_final_chunk" }
        });
        if let Some(model) = &self.response_model {
            record["model"] = Value::String(model.clone());
        }
        if let Some(usage) = &self.upstream_usage {
            record["usage"] = usage.clone();
        }
        record
    }

    fn error_record(&self, message: &str) -> Value {
        json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "requestId": self.request_id,
            "provider": self.provider,
            "modelName": self.model,
            "streaming": true,
            "error": {"message": message}
        })
    }
}

fn is_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
}

fn generated_tool_id() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let suffix: String = (0..24)
        .map(|_| CHARS[rand::rng().random_range(0..CHARS.len())] as char)
        .collect();
    format!("toolu_{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chunked_crlf_sse() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"event: message\r\ndata: {\"a\":");
        assert!(decoder.next_data().is_none());
        decoder.push(b"1}\r\n\r\n");
        assert_eq!(decoder.next_data().as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn skips_heartbeat_and_event_only_frames() {
        let mut decoder = SseDecoder::default();
        decoder.push(b": ping\n\nevent: keepalive\n\ndata: {\"a\":1}\n\n");
        assert_eq!(decoder.next_data().as_deref(), Some("{\"a\":1}"));
        assert!(decoder.next_data().is_none());
    }

    #[test]
    fn matches_shared_stream_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../bench/fixtures/stream-conversion.json"))
                .unwrap();
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        let mut events = fixture["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|chunk| state.process_chunk(chunk).unwrap())
            .collect::<Vec<_>>();
        events.extend(state.finish().unwrap());
        let event_types = events
            .iter()
            .map(|event| event["type"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            event_types.as_slice(),
            fixture["eventTypes"].as_array().unwrap().as_slice()
        );
        assert_eq!(events[0]["message"]["usage"], fixture["initialUsage"]);
        let delta_types = events
            .iter()
            .filter_map(|event| event.pointer("/delta/type").cloned())
            .collect::<Vec<_>>();
        assert_eq!(
            delta_types.as_slice(),
            fixture["deltaTypes"].as_array().unwrap().as_slice()
        );
        let final_usage = events
            .iter()
            .find(|event| event["type"] == "message_delta")
            .map(|event| &event["usage"])
            .unwrap();
        assert_eq!(final_usage, &fixture["finalUsage"]);

        let mut tools = StreamState::new("model".into(), "provider".into(), "request".into());
        let mut tool_events = fixture["toolChunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|chunk| tools.process_chunk(chunk).unwrap())
            .collect::<Vec<_>>();
        tool_events.extend(tools.finish().unwrap());
        let tool_event_types = tool_events
            .iter()
            .map(|event| event["type"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            tool_event_types.as_slice(),
            fixture["toolEventTypes"].as_array().unwrap().as_slice()
        );
        assert_eq!(tool_events[1]["content_block"]["name"], "lookup");
        assert_eq!(tool_events[2]["delta"]["partial_json"], "{\"id\":1}");
    }

    #[test]
    fn streams_parallel_tools_then_final_usage() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        let events = state.process_chunk(&json!({
            "model": "actual",
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "toolu_1", "function": {"name": "first", "arguments": "{\"a\":"}},
                {"index": 1, "id": "toolu_2", "function": {"name": "second", "arguments": "{\"b\":"}}
            ]}, "finish_reason": null}]
        })).unwrap();
        assert_eq!(events[0]["type"], "message_start");
        assert_eq!(events.len(), 1);

        let finished = state
            .process_chunk(&json!({
                "choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "1}"}},
                    {"index": 1, "function": {"arguments": "2}"}}
                ]}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
            }))
            .unwrap();
        assert!(finished.is_empty());
        let final_events = state.finish().unwrap();
        assert_eq!(final_events[0]["content_block"]["id"], "toolu_1");
        assert_eq!(
            final_events[2],
            json!({"type": "content_block_stop", "index": 0})
        );
        assert_eq!(final_events[3]["content_block"]["id"], "toolu_2");
        assert_eq!(
            final_events[5],
            json!({"type": "content_block_stop", "index": 1})
        );
        assert_eq!(final_events[6]["delta"]["stop_reason"], "tool_use");
        assert_eq!(final_events[6]["usage"]["input_tokens"], 10);
        assert_eq!(state.usage_record()["usage"]["total_tokens"], 14);
    }

    #[test]
    fn omits_reasoning_and_uses_actual_finish_reason() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        let events = state
            .process_chunk(&json!({
                "choices": [{"delta": {"reasoning_content": "private", "content": "answer"}, "finish_reason": "length"}]
            }))
            .unwrap();
        assert!(
            events
                .iter()
                .all(|event| event.pointer("/delta/type") != Some(&json!("thinking_delta")))
        );
        let final_events = state.finish().unwrap();
        assert_eq!(final_events[0]["delta"]["stop_reason"], "max_tokens");
    }

    #[test]
    fn streams_refusal_as_text_with_stop_details() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        let events = state
            .process_chunk(&json!({
                "choices": [{"delta": {"refusal": "Cannot comply."}, "finish_reason": "content_filter"}]
            }))
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.pointer("/delta/text") == Some(&json!("Cannot comply.")))
        );
        let final_events = state.finish().unwrap();
        assert_eq!(final_events[0]["delta"]["stop_reason"], "refusal");
        assert_eq!(
            final_events[0]["delta"]["stop_details"]["explanation"],
            "Cannot comply."
        );
    }

    #[test]
    fn supplies_content_filter_placeholder_when_upstream_sends_no_text() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        state
            .process_chunk(&json!({
                "choices": [{"delta": {"reasoning_content": "private"}, "finish_reason": "content_filter"}]
            }))
            .unwrap();
        let events = state.finish().unwrap();
        assert!(events.iter().any(|event| {
            event.pointer("/delta/text")
                == Some(&json!("Response withheld by the upstream content filter."))
        }));
        assert_eq!(
            events.last().unwrap()["type"],
            "message_stop",
            "content-filter refusal remains a successful protocol response"
        );
    }

    #[test]
    fn rejects_reasoning_only_and_malformed_streamed_tool_arguments() {
        let mut reasoning = StreamState::new("model".into(), "provider".into(), "request".into());
        reasoning
            .process_chunk(&json!({
                "choices": [{"delta": {"reasoning": "private"}, "finish_reason": "stop"}]
            }))
            .unwrap();
        assert!(reasoning.finish().is_err());

        for arguments in ["not json", "[]"] {
            let mut tools = StreamState::new("model".into(), "provider".into(), "request".into());
            tools
                .process_chunk(&json!({
                    "choices": [{"delta": {"tool_calls": [{
                        "index": 0,
                        "id": "toolu_1",
                        "function": {"name": "lookup", "arguments": arguments}
                    }]}, "finish_reason": "tool_calls"}]
                }))
                .unwrap();
            assert!(tools.finish().is_err());
        }

        let mut missing_name =
            StreamState::new("model".into(), "provider".into(), "request".into());
        missing_name
            .process_chunk(&json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "toolu_1", "function": {"arguments": "{}"}
                }]}, "finish_reason": "tool_calls"}]
            }))
            .unwrap();
        assert!(missing_name.finish().is_err());

        let mut mismatched = StreamState::new("model".into(), "provider".into(), "request".into());
        mismatched
            .process_chunk(&json!({
                "choices": [{"delta": {"content": "ok"}, "finish_reason": "tool_calls"}]
            }))
            .unwrap();
        assert!(mismatched.finish().is_err());
    }

    #[test]
    fn distinguishes_missing_usage_from_real_zero() {
        let mut missing = StreamState::new("model".into(), "provider".into(), "request".into());
        missing
            .process_chunk(
                &json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}),
            )
            .unwrap();
        let missing_final = missing.finish().unwrap();
        let missing_usage = &missing_final
            .iter()
            .find(|event| event["type"] == "message_delta")
            .unwrap()["usage"];
        assert_eq!(missing_usage["input_tokens"], Value::Null);
        assert_eq!(missing_usage["output_tokens"], Value::Null);
        assert_eq!(missing.usage_record()["usageStatus"], "missing_final_chunk");

        let mut partial = StreamState::new("model".into(), "provider".into(), "request".into());
        partial
            .process_chunk(&json!({
                "choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}],
                "usage": {"completion_tokens": 2}
            }))
            .unwrap();
        assert_eq!(
            partial.finish().unwrap()[0]["usage"]["input_tokens"],
            Value::Null
        );

        let mut zero = StreamState::new("model".into(), "provider".into(), "request".into());
        zero.process_chunk(
            &json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}),
        )
        .unwrap();
        zero.process_chunk(
            &json!({"choices": [], "usage": {"prompt_tokens": 0, "completion_tokens": 0}}),
        )
        .unwrap();
        assert_eq!(zero.finish().unwrap()[0]["usage"]["input_tokens"], 0);
    }

    #[test]
    fn buffers_tool_arguments_until_the_name_arrives() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        let initial = state
            .process_chunk(&json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "toolu_1", "function": {"name": "look", "arguments": "{\"value\":"}
                }]}, "finish_reason": null}]
            }))
            .unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0]["type"], "message_start");

        let named = state
            .process_chunk(&json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": 0, "function": {"name": "up", "arguments": "1}"}
                }]}, "finish_reason": "tool_calls"}]
            }))
            .unwrap();
        assert!(named.is_empty());
        let finished = state.finish().unwrap();
        assert_eq!(finished[0]["content_block"]["name"], "lookup");
        assert_eq!(finished[1]["delta"]["partial_json"], "{\"value\":1}");
    }

    #[test]
    fn rejects_tool_content_with_a_non_tool_finish_reason() {
        let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
        state
            .process_chunk(&json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "toolu_1",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]}, "finish_reason": null}]
            }))
            .unwrap();
        let during = state
            .process_chunk(&json!({
                "choices": [{"delta": {"content": "after tool"}, "finish_reason": "stop"}]
            }))
            .unwrap();
        assert!(during.is_empty());

        assert!(state.finish().is_err());
    }

    #[test]
    fn rejects_empty_choice_chunks_without_usage_and_malformed_usage() {
        for chunk in [
            json!({"choices": []}),
            json!({"choices": [], "usage": {}}),
            json!({"choices": [], "usage": "bad"}),
            json!({"choices": [], "usage": {"prompt_tokens": "1"}}),
            json!({"choices": [{"delta": {"role": "user"}, "finish_reason": null}]}),
            json!({"choices": [{"delta": {}, "finish_reason": 1}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "type": "custom"}]}, "finish_reason": null}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": 1}]}, "finish_reason": null}]}),
        ] {
            let mut state = StreamState::new("model".into(), "provider".into(), "request".into());
            assert!(
                state.process_chunk(&chunk).is_err(),
                "chunk should fail: {chunk}"
            );
        }
    }
}

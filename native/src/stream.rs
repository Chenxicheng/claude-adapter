use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    fmt,
    time::Duration,
};

use async_stream::stream;
use axum::{
    body::{Body, Bytes},
    response::{IntoResponse, Sse, sse::Event},
};
use chrono::Utc;
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use crate::{
    converter::{map_finish_reason, reasoning_content},
    storage::Storage,
};

const FIRST_BODY_BYTES_TIMEOUT: Duration = Duration::from_secs(120);
const FIRST_ANTHROPIC_EVENT_TIMEOUT: Duration = Duration::from_secs(240);
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
struct StreamTimeouts {
    first_body_bytes: Duration,
    first_anthropic_event: Duration,
    body_idle: Duration,
}

const STREAM_TIMEOUTS: StreamTimeouts = StreamTimeouts {
    first_body_bytes: FIRST_BODY_BYTES_TIMEOUT,
    first_anthropic_event: FIRST_ANTHROPIC_EVENT_TIMEOUT,
    body_idle: BODY_IDLE_TIMEOUT,
};

pub fn transform_stream(
    response: reqwest::Response,
    model: String,
    provider: String,
    storage: Storage,
    request_id: String,
    headers_received_at: Instant,
    tool_names: Vec<String>,
) -> Body {
    transform_bytes_with_timeouts(
        response.bytes_stream(),
        StreamState::new(model, provider, request_id, tool_names),
        storage,
        headers_received_at,
        STREAM_TIMEOUTS,
    )
}

#[cfg(test)]
fn transform_bytes<S, E>(upstream: S, state: StreamState, storage: Storage) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    transform_bytes_with_timeouts(upstream, state, storage, Instant::now(), STREAM_TIMEOUTS)
}

fn transform_bytes_with_timeouts<S, E>(
    upstream: S,
    mut state: StreamState,
    storage: Storage,
    headers_received_at: Instant,
    timeouts: StreamTimeouts,
) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let output = stream! {
        let upstream = timeout_bytes(upstream, headers_received_at, timeouts);
        let events = upstream.eventsource();
        futures_util::pin_mut!(events);
        let mut failed = false;
        let mut done = false;
        let first_event_deadline = headers_received_at + timeouts.first_anthropic_event;
        let mut waiting_for_first_event = true;
        loop {
            let event = if waiting_for_first_event {
                match timeout_at(first_event_deadline, events.next()).await {
                    Ok(event) => event,
                    Err(_) => {
                        let message = "Upstream stream timed out waiting for first Anthropic event";
                        storage.record_error(state.error_record(message));
                        yield Ok::<Event, Infallible>(encode_event(error_event(message)));
                        failed = true;
                        break;
                    }
                }
            } else {
                events.next().await
            };
            let Some(event) = event else { break };
            let result = match event {
                Ok(event) if event.data == "[DONE]" => {
                    done = true;
                    break;
                }
                Ok(event) => serde_json::from_str::<Value>(&event.data)
                    .map_err(|error| format!("Invalid upstream SSE JSON: {error}"))
                    .and_then(|chunk| state.process_chunk(&chunk)),
                Err(error) => Err(format!("Upstream stream failed: {error}")),
            };
            match result {
                Ok(events) => {
                    if !events.is_empty() {
                        waiting_for_first_event = false;
                    }
                    for event in events { yield Ok::<Event, Infallible>(encode_event(event)); }
                }
                Err(message) => {
                    storage.record_error(state.error_record(&message));
                    yield Ok(encode_event(error_event(&message)));
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            match state.finish(done) {
                Ok(events) => {
                    storage.record_usage(state.usage_record());
                    for event in events { yield Ok(encode_event(event)); }
                }
                Err(message) => {
                    storage.record_error(state.error_record(&message));
                    yield Ok(encode_event(error_event(&message)));
                }
            }
        }
    };
    Sse::new(output).into_response().into_body()
}

#[derive(Debug)]
struct StreamReadError(String);

impl fmt::Display for StreamReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StreamReadError {}

fn timeout_bytes<S, E>(
    upstream: S,
    headers_received_at: Instant,
    timeouts: StreamTimeouts,
) -> impl Stream<Item = Result<Bytes, StreamReadError>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send,
    E: std::error::Error + Send + Sync,
{
    stream! {
        futures_util::pin_mut!(upstream);
        let mut first = true;
        let mut deadline = headers_received_at + timeouts.first_body_bytes;
        loop {
            match timeout_at(deadline, upstream.next()).await {
                Ok(Some(Ok(bytes))) if bytes.is_empty() => {}
                Ok(Some(Ok(bytes))) => {
                    first = false;
                    yield Ok(bytes);
                    deadline = Instant::now() + timeouts.body_idle;
                }
                Ok(Some(Err(error))) => {
                    yield Err(StreamReadError(error.to_string()));
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    let message = if first {
                        "Upstream stream timed out waiting for first body bytes"
                    } else {
                        "Upstream stream was idle for too long"
                    };
                    yield Err(StreamReadError(message.into()));
                    break;
                }
            }
        }
    }
}

fn encode_event(event: Value) -> Event {
    Event::default()
        .event(event["type"].as_str().expect("internal event type"))
        .json_data(&event)
        .expect("JSON value serialization")
}

fn error_event(message: &str) -> Value {
    json!({"type": "error", "error": {"type": "api_error", "message": message}})
}

struct ToolCall {
    id: String,
    name: String,
    arguments: String,
    emitted_arguments: usize,
    block_index: Option<usize>,
    closed: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ContentKind {
    Text,
    Thinking,
}

struct StreamState {
    message_id: String,
    request_id: String,
    model: String,
    response_model: Option<String>,
    provider: String,
    next_index: usize,
    tools: BTreeMap<usize, ToolCall>,
    tool_names: Vec<String>,
    used_tool_ids: HashSet<String>,
    active_content: Option<(usize, ContentKind)>,
    started: bool,
    content_seen: bool,
    refusal_text: String,
    finish_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    upstream_usage: Option<Value>,
    error_details: Option<Value>,
}

impl StreamState {
    fn new(model: String, provider: String, request_id: String, tool_names: Vec<String>) -> Self {
        Self {
            message_id: format!("msg_{}", Utc::now().timestamp_millis()),
            request_id,
            model,
            response_model: None,
            provider,
            next_index: 0,
            tools: BTreeMap::new(),
            tool_names,
            used_tool_ids: HashSet::new(),
            active_content: None,
            started: false,
            content_seen: false,
            refusal_text: String::new(),
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
            upstream_usage: None,
            error_details: None,
        }
    }

    fn process_chunk(&mut self, chunk: &Value) -> Result<Vec<Value>, String> {
        let mut events = Vec::new();
        if let Some(error) = chunk.get("error") {
            self.error_details = Some(error.clone());
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string()));
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            let object = usage
                .as_object()
                .ok_or("Upstream stream usage must be an object or null")?;
            if object.contains_key("prompt_tokens") || object.contains_key("completion_tokens") {
                let token = |name: &str| -> Result<u64, String> {
                    match object.get(name) {
                        None | Some(Value::Null) => Ok(0),
                        Some(value) => value.as_u64().ok_or_else(|| {
                            format!("Upstream stream usage.{name} must be an integer")
                        }),
                    }
                };
                self.input_tokens = Some(token("prompt_tokens")?);
                self.output_tokens = Some(token("completion_tokens")?);
            }
            if !object.is_empty() {
                self.upstream_usage = Some(usage.clone());
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
            .ok_or("Upstream stream chunk is missing choices")?;
        if choices.is_empty() {
            return Ok(events);
        }
        if choices.len() != 1 {
            return Err("Upstream stream chunk must contain exactly one choice".into());
        }
        let choice = &choices[0];
        let delta = choice
            .get("delta")
            .and_then(Value::as_object)
            .ok_or("Upstream stream choice is missing delta")?;
        if delta
            .get("role")
            .is_some_and(|role| !role.is_null() && role.as_str() != Some("assistant"))
        {
            return Err("Upstream stream delta.role must be assistant or null".into());
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
        if delta
            .get("tool_calls")
            .is_some_and(|calls| !calls.is_null() && !calls.is_array())
        {
            return Err("Upstream stream delta.tool_calls must be an array".into());
        }
        if let Some(finished) = self.finish_reason.as_deref() {
            if let Some(reason) = choice
                .get("finish_reason")
                .filter(|reason| !reason.is_null())
            {
                let reason = reason
                    .as_str()
                    .ok_or("Upstream stream finish_reason must be a string or null")?;
                if reason != finished {
                    return Err("Upstream stream sent a conflicting finish_reason".into());
                }
            }
            let has_content = delta.iter().any(|(field, value)| match field.as_str() {
                "role" => false,
                "content" | "refusal" | "reasoning_content" | "reasoning" => {
                    value.as_str().is_some_and(|text| !text.is_empty())
                }
                "tool_calls" => value.as_array().is_some_and(|calls| !calls.is_empty()),
                _ => !value.is_null(),
            });
            if has_content {
                return Err("Upstream stream sent content after finish_reason".into());
            }
            return Ok(events);
        }
        if !self.started {
            events.push(json!({"type": "message_start", "message": {
                "id": self.message_id, "type": "message", "role": "assistant", "content": [],
                "model": self.model, "stop_reason": Value::Null, "stop_sequence": Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }}));
            self.started = true;
        }
        let reasoning = reasoning_content(&choice["delta"]);
        // Match main: reasoning after tools is not replayed as a new thinking block.
        if !reasoning.is_empty() && self.tools.is_empty() {
            self.push_content(reasoning, ContentKind::Thinking, &mut events);
        }
        for (field, kind) in [
            ("content", ContentKind::Text),
            ("refusal", ContentKind::Text),
        ] {
            if let Some(text) = delta
                .get(field)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                if field == "refusal" {
                    self.refusal_text.push_str(text);
                }
                self.push_content(text, kind, &mut events);
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.process_tool_delta(call, &mut events)?;
            }
        }
        if let Some(reason) = choice
            .get("finish_reason")
            .filter(|reason| !reason.is_null())
        {
            self.finish_reason = Some(
                reason
                    .as_str()
                    .ok_or("Upstream stream finish_reason must be a string or null")?
                    .to_owned(),
            );
            self.close_blocks(&mut events)?;
        }
        Ok(events)
    }

    fn process_tool_delta(&mut self, delta: &Value, events: &mut Vec<Value>) -> Result<(), String> {
        let delta = delta
            .as_object()
            .ok_or("Upstream tool delta must be an object")?;
        let index = delta
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or("Upstream tool delta is missing index")?;
        if delta
            .get("type")
            .is_some_and(|kind| !kind.is_null() && kind.as_str() != Some("function"))
        {
            return Err("Upstream tool delta type must be function or null".into());
        }
        if delta
            .get("id")
            .is_some_and(|id| !id.is_null() && !id.is_string())
        {
            return Err("Upstream tool delta id must be a string or null".into());
        }
        if delta
            .get("function")
            .is_some_and(|function| !function.is_null() && !function.is_object())
        {
            return Err("Upstream tool delta function must be an object or null".into());
        }
        self.close_content(events);
        let tool = self.tools.entry(index).or_insert_with(|| ToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            emitted_arguments: 0,
            block_index: None,
            closed: false,
        });
        if let Some(id) = delta
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            // Identity is fixed when the block starts, including repaired IDs (main parity).
            if tool.block_index.is_none() {
                tool.id = id.to_owned();
            }
        }
        let function = delta.get("function").and_then(Value::as_object);
        for (field, target) in [("name", &mut tool.name), ("arguments", &mut tool.arguments)] {
            if let Some(fragment) = function
                .and_then(|function| function.get(field))
                .filter(|v| !v.is_null())
            {
                let fragment = fragment.as_str().ok_or_else(|| {
                    format!("Upstream tool delta function.{field} must be a string")
                })?;
                if field == "name" && !fragment.is_empty() && tool.block_index.is_some() {
                    if fragment != target.as_str() {
                        return Err(
                            "Upstream tool function.name changed after streaming started".into(),
                        );
                    }
                    continue;
                }
                target.push_str(fragment);
            }
        }
        let stable = self.tool_names.iter().any(|name| name == &tool.name)
            && !self
                .tool_names
                .iter()
                .any(|name| name != &tool.name && name.starts_with(&tool.name));
        if tool.block_index.is_some() || stable {
            self.emit_tool(index, events)?;
        }
        self.content_seen = true;
        Ok(())
    }

    fn emit_tool(&mut self, index: usize, events: &mut Vec<Value>) -> Result<(), String> {
        let tool = self.tools.get_mut(&index).expect("known tool");
        if tool.block_index.is_none() {
            if !self.tool_names.contains(&tool.name) {
                return Err("Upstream tool call has a missing or undeclared function.name".into());
            }
            while tool.id.is_empty() || self.used_tool_ids.contains(&tool.id) {
                tool.id = generated_tool_id();
            }
            self.used_tool_ids.insert(tool.id.clone());
            let index = self.next_index;
            self.next_index += 1;
            tool.block_index = Some(index);
            events.push(json!({"type": "content_block_start", "index": index,
                "content_block": {"type": "tool_use", "id": tool.id, "name": tool.name, "input": {}}}));
        }
        if tool.emitted_arguments < tool.arguments.len() {
            events.push(json!({"type": "content_block_delta", "index": tool.block_index,
                "delta": {"type": "input_json_delta", "partial_json": &tool.arguments[tool.emitted_arguments..]}}));
            tool.emitted_arguments = tool.arguments.len();
        }
        Ok(())
    }

    fn push_content(&mut self, text: &str, kind: ContentKind, events: &mut Vec<Value>) {
        if self
            .active_content
            .is_some_and(|(_, active)| active != kind)
        {
            self.close_content(events);
        }
        let index = if let Some((index, _)) = self.active_content {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            let block = match kind {
                ContentKind::Text => json!({"type": "text", "text": ""}),
                ContentKind::Thinking => json!({"type": "thinking", "thinking": ""}),
            };
            events.push(
                json!({"type": "content_block_start", "index": index, "content_block": block}),
            );
            self.active_content = Some((index, kind));
            index
        };
        let delta = match kind {
            ContentKind::Text => json!({"type": "text_delta", "text": text}),
            ContentKind::Thinking => json!({"type": "thinking_delta", "thinking": text}),
        };
        events.push(json!({"type": "content_block_delta", "index": index, "delta": delta}));
        self.content_seen = true;
    }

    fn close_content(&mut self, events: &mut Vec<Value>) {
        if let Some((index, _)) = self.active_content.take() {
            events.push(json!({"type": "content_block_stop", "index": index}));
        }
    }

    fn close_blocks(&mut self, events: &mut Vec<Value>) -> Result<(), String> {
        let reason = self.finish_reason.as_deref().expect("terminal reason");
        map_finish_reason(reason).map_err(|error| error.message)?;
        if (reason == "tool_calls") != !self.tools.is_empty() && reason != "length" {
            return Err("Upstream tool calls do not match finish_reason".into());
        }
        if reason == "content_filter" && !self.content_seen {
            self.push_content(
                "Response withheld by the upstream content filter.",
                ContentKind::Text,
                events,
            );
        }
        self.close_content(events);
        let indices = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.closed)
            .map(|(index, _)| *index)
            .collect::<Vec<_>>();
        for index in indices {
            let tool = &self.tools[&index];
            if !tool.arguments.is_empty() {
                let input: Value = serde_json::from_str(&tool.arguments).map_err(|error| {
                    format!("Upstream tool arguments are invalid JSON: {error}")
                })?;
                if !input.is_object() {
                    return Err("Upstream tool arguments must decode to a JSON object".into());
                }
            }
            self.emit_tool(index, events)?;
            let tool = self.tools.get_mut(&index).expect("known tool");
            events.push(json!({"type": "content_block_stop", "index": tool.block_index}));
            tool.closed = true;
            tool.arguments = String::new();
        }
        Ok(())
    }

    fn finish(&mut self, done: bool) -> Result<Vec<Value>, String> {
        if !self.started {
            return Err("Upstream stream ended without a message".into());
        }
        let mut events = Vec::new();
        if self.finish_reason.is_none() {
            if !done || !self.content_seen {
                return Err("Upstream stream ended without a finish_reason".into());
            }
            self.finish_reason = Some(
                if self.tools.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
                }
                .into(),
            );
            self.close_blocks(&mut events)?;
        }
        let reason = self.finish_reason.as_deref().expect("terminal reason");
        let stop_reason = if !self.refusal_text.is_empty() {
            "refusal"
        } else {
            map_finish_reason(reason).map_err(|error| error.message)?
        };
        let mut delta = json!({"stop_reason": stop_reason, "stop_sequence": Value::Null});
        if stop_reason == "refusal" {
            delta["stop_details"] = json!({"type": "refusal", "category": Value::Null,
                "explanation": if self.refusal_text.is_empty() { "Response withheld by the upstream content filter." } else { &self.refusal_text }});
        }
        let mut usage = json!({"output_tokens": self.output_tokens.unwrap_or(0)});
        if let Some(input) = self.input_tokens {
            usage["input_tokens"] = json!(input);
        }
        events.push(json!({"type": "message_delta", "delta": delta, "usage": usage}));
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
            "outcome": if self.finish_reason.as_deref() == Some("stop") && !self.content_seen {
                "empty_end_turn"
            } else {
                "usable"
            },
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
        let mut error = json!({"message": message});
        if let Some(details) = &self.error_details {
            error["response"] = json!({"error": details});
        }
        json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "requestId": self.request_id,
            "provider": self.provider,
            "modelName": self.model,
            "streaming": true,
            "error": error
        })
    }
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
    use std::{io, time::Duration};

    fn state(names: &[&str]) -> StreamState {
        StreamState::new(
            "model".into(),
            "provider".into(),
            "request".into(),
            names.iter().map(|name| (*name).into()).collect(),
        )
    }
    fn chunk(delta: Value, reason: Option<&str>) -> Value {
        json!({"choices": [{"delta": delta, "finish_reason": reason}]})
    }
    fn call(index: usize, name: &str, arguments: &str) -> Value {
        json!({"index": index, "id": format!("call_{index}"), "type": "function", "function": {"name": name, "arguments": arguments}})
    }
    fn check_lifecycle(events: &[Value]) {
        let mut open = HashSet::new();
        let mut next = 0;
        for event in events {
            match event["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert_eq!(event["index"], next);
                    assert!(open.insert(next));
                    next += 1;
                }
                "content_block_delta" => {
                    assert!(open.contains(&event["index"].as_u64().unwrap()));
                }
                "content_block_stop" => {
                    assert!(open.remove(&event["index"].as_u64().unwrap()));
                }
                "message_stop" => assert!(open.is_empty()),
                _ => {}
            }
        }
        assert!(open.is_empty());
    }

    fn timeouts(first_body: u64, first_event: u64, idle: u64) -> StreamTimeouts {
        StreamTimeouts {
            first_body_bytes: Duration::from_secs(first_body),
            first_anthropic_event: Duration::from_secs(first_event),
            body_idle: Duration::from_secs(idle),
        }
    }

    async fn render_timed<S, E>(upstream: S, timeouts: StreamTimeouts) -> String
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let (storage, writer) = Storage::start(dir.path().into());
        let body =
            transform_bytes_with_timeouts(upstream, state(&[]), storage, Instant::now(), timeouts);
        let result = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        writer.await.unwrap();
        String::from_utf8(result.to_vec()).unwrap()
    }

    #[test]
    fn matches_shared_fixture_and_main_usage() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../bench/fixtures/stream-conversion.json"))
                .unwrap();
        for (chunks, types) in [("chunks", "eventTypes"), ("toolChunks", "toolEventTypes")] {
            let mut state = state(&["lookup"]);
            let mut events = Vec::new();
            for chunk in fixture[chunks].as_array().unwrap() {
                events.extend(state.process_chunk(chunk).unwrap());
            }
            events.extend(state.finish(true).unwrap());
            check_lifecycle(&events);
            assert_eq!(
                events
                    .iter()
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>(),
                *fixture[types].as_array().unwrap()
            );
            assert_eq!(events[0]["message"]["usage"], fixture["initialUsage"]);
            if chunks == "chunks" {
                assert_eq!(events[events.len() - 2]["usage"], fixture["finalUsage"]);
            }
        }
    }

    #[test]
    fn emits_thinking_fallback_and_reasoning_only() {
        for empty in [Value::Null, json!("")] {
            let mut state = state(&[]);
            let mut events = state
                .process_chunk(&chunk(
                    json!({"reasoning_content": empty, "reasoning": "思考"}),
                    Some("length"),
                ))
                .unwrap();
            assert_eq!(
                events[2]["delta"],
                json!({"type": "thinking_delta", "thinking": "思考"})
            );
            events.extend(state.finish(false).unwrap());
            assert_eq!(
                events[events.len() - 2]["delta"]["stop_reason"],
                "max_tokens"
            );
            check_lifecycle(&events);
        }
    }

    #[test]
    fn streams_interleaved_tools_immediately_and_preserves_following_text() {
        let mut state = state(&["first", "second"]);
        let mut events = state
            .process_chunk(&chunk(
                json!({"tool_calls": [call(9,"second","{"), call(2,"first","{")]}),
                None,
            ))
            .unwrap();
        assert_eq!(events.len(), 5);
        assert_eq!(events[1]["content_block"]["name"], "second");
        events.extend(
            state
                .process_chunk(&chunk(
                    json!({"tool_calls": [
                        {"index":2,"function":{"arguments":"\"a\":1}"}},
                        {"index":9,"function":{"arguments":"\"b\":2}"}}
                    ]}),
                    None,
                ))
                .unwrap(),
        );
        let text = state
            .process_chunk(&chunk(json!({"content":"after tool"}), Some("tool_calls")))
            .unwrap();
        assert_eq!(text[0]["index"], 2);
        assert_eq!(text[1]["delta"]["text"], "after tool");
        events.extend(text);
        events.extend(state.finish(true).unwrap());
        check_lifecycle(&events);
        assert!(state.tools.values().all(|tool| tool.arguments.is_empty()));
    }

    #[test]
    fn buffers_only_ambiguous_or_incomplete_names() {
        let mut state = state(&["look", "lookup", "other"]);
        let first = state
            .process_chunk(&chunk(
                json!({"tool_calls":[call(0,"look","{}"),call(1,"other","")]}),
                None,
            ))
            .unwrap();
        assert_eq!(first[1]["content_block"]["name"], "other");
        assert!(state.tools[&0].block_index.is_none());
        let rest = state
            .process_chunk(&chunk(
                json!({"tool_calls":[{"index":0,"function":{"name":"up"}}]}),
                Some("tool_calls"),
            ))
            .unwrap();
        assert_eq!(rest[0]["content_block"]["name"], "lookup");
        let mut events = first;
        events.extend(rest);
        events.extend(state.finish(true).unwrap());
        check_lifecycle(&events);
        let mut ambiguous = super::tests::state(&["look", "lookup"]);
        ambiguous
            .process_chunk(&chunk(json!({"tool_calls":[call(0,"look","")]}), None))
            .unwrap();
        assert_eq!(
            ambiguous.finish(true).unwrap()[0]["content_block"]["name"],
            "look"
        );
    }

    #[test]
    fn accepts_repeated_tool_name_and_rejects_changed_name() {
        let mut stream_state = state(&["lookup", "other"]);
        let mut events = stream_state
            .process_chunk(&chunk(json!({"tool_calls":[call(0,"lookup","{")]}), None))
            .unwrap();
        events.extend(
            stream_state
                .process_chunk(&chunk(
                    json!({"tool_calls":[{"index":0,"function":{"name":"lookup","arguments":"\"id\":1}"}}]}),
                    Some("tool_calls"),
                ))
                .unwrap(),
        );
        events.extend(stream_state.finish(true).unwrap());
        check_lifecycle(&events);
        assert_eq!(stream_state.tools[&0].name, "lookup");
        assert_eq!(
            events
                .iter()
                .filter(|event| event["delta"]["type"] == "input_json_delta")
                .count(),
            2
        );

        let mut changed = state(&["lookup", "other"]);
        changed
            .process_chunk(&chunk(json!({"tool_calls":[call(0,"lookup","{")]}), None))
            .unwrap();
        assert!(
            changed
                .process_chunk(&chunk(
                    json!({"tool_calls":[{"index":0,"function":{"name":"other"}}]}),
                    None,
                ))
                .unwrap_err()
                .contains("name changed")
        );
    }

    #[test]
    fn validates_tools_and_terminal_state_without_false_success() {
        for (name, args) in [
            ("lookup", "[1]"),
            ("lookup", "{"),
            ("unknown", "{}"),
            ("", "{}"),
        ] {
            let mut state = state(&["lookup"]);
            assert!(
                state
                    .process_chunk(&chunk(
                        json!({"tool_calls":[call(0,name,args)]}),
                        Some("tool_calls")
                    ))
                    .is_err()
            );
        }
        let mut state = state(&[]);
        state
            .process_chunk(&chunk(json!({"content":"partial"}), None))
            .unwrap();
        assert!(state.finish(false).is_err());
        let events = state.finish(true).unwrap();
        assert_eq!(events.last().unwrap()["type"], "message_stop");
        assert!(
            state
                .process_chunk(&chunk(json!({"content":"late"}), None))
                .is_err()
        );
    }

    #[test]
    fn preserves_and_classifies_legal_empty_end_turn() {
        let mut state = state(&[]);
        let mut events = state
            .process_chunk(&chunk(json!({}), Some("stop")))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "message_start");
        events.extend(state.finish(true).unwrap());
        assert_eq!(events[1]["delta"]["stop_reason"], "end_turn");
        assert_eq!(events[2]["type"], "message_stop");
        assert_eq!(state.usage_record()["outcome"], "empty_end_turn");
    }

    #[test]
    fn distinguishes_usage_missing_zero_partial_and_usage_only() {
        let mut state = state(&[]);
        assert!(
            state
                .process_chunk(&json!({"choices":[]}))
                .unwrap()
                .is_empty()
        );
        state
            .process_chunk(&chunk(json!({"content":"ok"}), Some("stop")))
            .unwrap();
        let missing = state.finish(false).unwrap();
        assert_eq!(missing[0]["usage"], json!({"output_tokens":0}));
        assert_eq!(state.usage_record()["usageStatus"], "missing_final_chunk");
        state
            .process_chunk(&json!({"choices":[],"usage":{"prompt_tokens":0,"completion_tokens":0}}))
            .unwrap();
        assert_eq!(
            state.finish(true).unwrap()[0]["usage"],
            json!({"input_tokens":0,"output_tokens":0})
        );
        state
            .process_chunk(&json!({"choices":[],"usage":{"completion_tokens":2}}))
            .unwrap();
        assert_eq!(
            state.finish(true).unwrap()[0]["usage"],
            json!({"input_tokens":0,"output_tokens":2})
        );
    }

    #[test]
    fn accepts_content_free_tails_but_rejects_post_finish_content_or_conflict() {
        let mut stream_state = state(&[]);
        let mut events = stream_state
            .process_chunk(&chunk(json!({"content":"answer"}), Some("stop")))
            .unwrap();
        for tail in [
            chunk(json!({}), None),
            chunk(
                json!({"role":"assistant","content":"","tool_calls":[]}),
                Some("stop"),
            ),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4}}),
        ] {
            assert!(stream_state.process_chunk(&tail).unwrap().is_empty());
        }
        events.extend(stream_state.finish(true).unwrap());
        check_lifecycle(&events);
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "message_stop")
                .count(),
            1
        );
        assert_eq!(
            events[events.len() - 2]["usage"],
            json!({"input_tokens":3,"output_tokens":4})
        );

        for tail in [
            chunk(json!({"content":"late"}), None),
            chunk(json!({"reasoning_content":"late"}), None),
            chunk(json!({"tool_calls":[{"index":0}]}), None),
            chunk(json!({}), Some("length")),
        ] {
            let mut rejected = state(&[]);
            rejected
                .process_chunk(&chunk(json!({"content":"answer"}), Some("stop")))
                .unwrap();
            assert!(rejected.process_chunk(&tail).is_err());
        }
    }

    #[test]
    fn preserves_errors_and_refusal() {
        let mut state = state(&[]);
        let error = state
            .process_chunk(
                &json!({"error":{"message":"Not Acceptable","code":"unsupported_field"}}),
            )
            .unwrap_err();
        assert_eq!(
            state.error_record(&error)["error"]["response"]["error"]["code"],
            "unsupported_field"
        );
        let events = state
            .process_chunk(&chunk(
                json!({"refusal":"Cannot comply"}),
                Some("content_filter"),
            ))
            .unwrap();
        assert_eq!(events[2]["delta"]["text"], "Cannot comply");
        assert_eq!(
            state.finish(true).unwrap()[0]["delta"]["stop_reason"],
            "refusal"
        );
    }

    #[tokio::test]
    async fn parses_all_line_endings_and_utf8_across_every_byte() {
        for separator in ["\n", "\r\n", "\r"] {
            let data = format!(
                ": ping{separator}{separator}data: {}{separator}{separator}data: [DONE]{separator}{separator}",
                chunk(json!({"content":"中文🙂"}), Some("stop"))
            );
            let upstream = futures_util::stream::iter(
                data.into_bytes()
                    .into_iter()
                    .map(|byte| Ok::<_, io::Error>(Bytes::from(vec![byte]))),
            );
            let dir = tempfile::tempdir().unwrap();
            let (storage, writer) = Storage::start(dir.path().into());
            let body = transform_bytes(upstream, state(&[]), storage);
            let result = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let text = String::from_utf8(result.to_vec()).unwrap();
            assert!(text.contains("中文🙂"), "{text}");
            assert!(text.contains("event: message_stop"), "{text}");
            assert!(!text.contains("event: error"), "{text}");
            writer.await.unwrap();
        }
    }

    #[tokio::test]
    async fn delivers_first_content_before_upstream_can_continue() {
        for delta in [
            json!({"reasoning_content":"think"}),
            json!({"content":"text"}),
            json!({"tool_calls":[call(0,"lookup","{")]}),
            json!({"tool_calls":[{"index":0,"function":{"name":"lookup","arguments":"{"}}]}),
        ] {
            let (resume, wait) = tokio::sync::oneshot::channel::<()>();
            let upstream = stream! {
                yield Ok::<_,io::Error>(Bytes::from(format!("data: {}\n\n",chunk(delta,None))));
                let _ = wait.await;
                yield Err(io::Error::other("deliberate disconnect"));
            };
            let dir = tempfile::tempdir().unwrap();
            let (storage, writer) = Storage::start(dir.path().into());
            let mut body =
                transform_bytes(upstream, state(&["lookup"]), storage).into_data_stream();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let bytes = body.next().await.unwrap().unwrap();
                    if String::from_utf8_lossy(&bytes).contains("event: content_block_delta") {
                        break;
                    }
                }
            })
            .await
            .expect("content must arrive before upstream is resumed");
            resume.send(()).unwrap();
            let mut tail = String::new();
            while let Some(bytes) = body.next().await {
                tail.push_str(&String::from_utf8_lossy(&bytes.unwrap()));
            }
            assert!(tail.contains("event: error"));
            assert!(!tail.contains("event: message_stop"));
            drop(body);
            writer.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_when_first_body_bytes_never_arrive() {
        let upstream = futures_util::stream::pending::<Result<Bytes, io::Error>>();
        let text = render_timed(upstream, timeouts(1, 2, 3)).await;

        assert!(
            text.contains("timed out waiting for first body bytes"),
            "{text}"
        );
        assert_eq!(text.matches("event: error").count(), 1, "{text}");
        assert!(!text.contains("event: message_stop"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn partial_bytes_and_comments_do_not_satisfy_first_event_deadline() {
        for first in [
            Bytes::from_static(b"data: {"),
            Bytes::from_static(b": ping\n\n"),
        ] {
            let upstream = stream! {
                yield Ok::<_, io::Error>(first);
                tokio::time::sleep(Duration::from_secs(3)).await;
                yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
            };
            let text = render_timed(upstream, timeouts(1, 2, 10)).await;

            assert!(
                text.contains("timed out waiting for first Anthropic event"),
                "{text}"
            );
            assert_eq!(text.matches("event: error").count(), 1, "{text}");
            assert!(!text.contains("event: message_stop"), "{text}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn first_byte_and_first_event_deadlines_share_the_headers_anchor() {
        let upstream = stream! {
            tokio::time::sleep(Duration::from_millis(900)).await;
            yield Ok::<_, io::Error>(Bytes::from_static(b"data: {"));
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            yield Ok(Bytes::from_static(b"}\n\n"));
        };
        let text = render_timed(upstream, timeouts(1, 2, 10)).await;

        assert!(
            text.contains("timed out waiting for first Anthropic event"),
            "{text}"
        );
        assert!(
            !text.contains("timed out waiting for first body bytes"),
            "{text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn body_idle_timeout_resets_after_each_non_empty_chunk() {
        let first = format!("data: {}\n\n", chunk(json!({"content":"a"}), None));
        let second = format!("data: {}\n\n", chunk(json!({"content":"b"}), Some("stop")));
        let upstream = stream! {
            yield Ok::<_, io::Error>(Bytes::from(first));
            tokio::time::sleep(Duration::from_secs(2)).await;
            yield Ok(Bytes::from(second));
            tokio::time::sleep(Duration::from_secs(2)).await;
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        };
        let text = render_timed(upstream, timeouts(1, 2, 3)).await;

        assert!(text.contains("event: message_stop"), "{text}");
        assert!(!text.contains("event: error"), "{text}");

        let stalled = stream! {
            yield Ok::<_, io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                chunk(json!({"content":"a"}), None)
            )));
            tokio::time::sleep(Duration::from_secs(4)).await;
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        };
        let text = render_timed(stalled, timeouts(1, 2, 3)).await;
        assert!(text.contains("stream was idle for too long"), "{text}");
        assert_eq!(text.matches("event: error").count(), 1, "{text}");
        assert!(!text.contains("event: message_stop"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn downstream_backpressure_does_not_consume_body_idle_budget() {
        let (resume, wait) = tokio::sync::oneshot::channel::<()>();
        let upstream = stream! {
            yield Ok::<_, io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                chunk(json!({"content":"a"}), None)
            )));
            let _ = wait.await;
            yield Ok(Bytes::from(format!(
                "data: {}\n\ndata: [DONE]\n\n",
                chunk(json!({}), Some("stop"))
            )));
        };
        let dir = tempfile::tempdir().unwrap();
        let (storage, writer) = Storage::start(dir.path().into());
        let mut body = transform_bytes_with_timeouts(
            upstream,
            state(&[]),
            storage,
            Instant::now(),
            timeouts(1, 2, 3),
        )
        .into_data_stream();
        loop {
            let bytes = body.next().await.unwrap().unwrap();
            if String::from_utf8_lossy(&bytes).contains("event: content_block_delta") {
                break;
            }
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        resume.send(()).unwrap();
        let mut tail = String::new();
        while let Some(bytes) = body.next().await {
            tail.push_str(&String::from_utf8_lossy(&bytes.unwrap()));
        }
        assert!(tail.contains("event: message_stop"), "{tail}");
        assert!(!tail.contains("event: error"), "{tail}");
        writer.await.unwrap();
    }
}

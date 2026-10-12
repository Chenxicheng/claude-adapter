use super::*;
use std::{io, time::Duration};

fn transform_bytes<S, E>(upstream: S, state: StreamState, storage: Storage) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    transform_bytes_with_timeouts(upstream, state, storage, Instant::now(), STREAM_TIMEOUTS)
}

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
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../bench/fixtures/stream-conversion.json"
    ))
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
fn length_with_tool_calls_requires_complete_arguments() {
    let mut complete = state(&["lookup"]);
    let events = complete
        .process_chunk(&chunk(
            json!({"tool_calls": [call(0, "lookup", "{\"id\":1}")]}),
            Some("length"),
        ))
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "content_block_stop")
    );
    let final_events = complete.finish(false).unwrap();
    assert_eq!(final_events[0]["delta"]["stop_reason"], "max_tokens");

    let mut truncated = state(&["lookup"]);
    assert!(
        truncated
            .process_chunk(&chunk(
                json!({"tool_calls": [call(0, "lookup", "{\"id\":")]}),
                Some("length"),
            ))
            .is_err()
    );
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
        .process_chunk(&json!({"error":{"message":"Not Acceptable","code":"unsupported_field"}}))
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
        let mut body = transform_bytes(upstream, state(&["lookup"]), storage).into_data_stream();
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

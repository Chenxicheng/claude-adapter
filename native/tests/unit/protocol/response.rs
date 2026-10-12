use super::*;

#[test]
fn matches_shared_response_fixtures() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../bench/fixtures/response-conversion.json"
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
        assert!(converted["usage"].get("output_tokens_details").is_none());
        assert_eq!(converted["usage"]["cache_read_input_tokens"], Value::Null);
    }
}

#[test]
fn preserves_legal_empty_end_turn() {
    let response = json!({
        "id": "chatcmpl_empty",
        "choices": [{
            "message": {"role": "assistant", "content": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3}
    });
    let converted = convert_response(&response, "model").unwrap();
    assert_eq!(converted["content"], json!([]));
    assert_eq!(converted["stop_reason"], "end_turn");
    assert_eq!(converted["usage"]["output_tokens"], 3);
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
fn rejects_malformed_tool_calls() {
    for response in [
        json!({
            "id": "chatcmpl_length",
            "choices": [{"message": {"role": "assistant", "tool_calls": [{
                "id": "toolu_1", "type": "function", "function": {"name": "lookup", "arguments": "{\"id\":"}
            }]}, "finish_reason": "length"}]
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
    assert_eq!(first["usage"]["input_tokens"], 0);
    assert_eq!(first["usage"]["output_tokens"], 0);

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

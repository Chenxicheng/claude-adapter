use std::{collections::HashMap, future::pending};

use super::*;

#[tokio::test(start_paused = true)]
async fn streaming_header_timeout_is_a_gateway_timeout() {
    let error = await_upstream_headers(
        pending::<reqwest::Result<reqwest::Response>>(),
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();

    assert_eq!(error.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        error.message,
        "Upstream timed out waiting for response headers"
    );
}

#[test]
fn custom_upstream_headers_are_preserved() {
    let config = AdapterConfig {
        base_url: "https://provider.test/v1".to_owned(),
        api_key: Some("secret".to_owned()),
        api_key_env: None,
        upstream_headers: Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://example.com".to_owned()),
            ("X-Title".to_owned(), "Claude Adapter".to_owned()),
            ("User-Agent".to_owned(), "Claude-Adapter/2.0".to_owned()),
        ])),
        upstream_capabilities: Default::default(),
    };

    let headers = build_default_headers(&config).unwrap();

    assert_eq!(headers["http-referer"], "https://example.com");
    assert_eq!(headers["x-title"], "Claude Adapter");
    assert_eq!(headers[header::USER_AGENT], "Claude-Adapter/2.0");
    assert_eq!(headers[header::ACCEPT], "application/json");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::AUTHORIZATION], "Bearer secret");
}

#[test]
fn default_user_agent_is_used_when_not_configured() {
    let config = AdapterConfig {
        base_url: "https://provider.test/v1".to_owned(),
        api_key: Some("secret".to_owned()),
        api_key_env: None,
        upstream_headers: None,
        upstream_capabilities: Default::default(),
    };

    let headers = build_default_headers(&config).unwrap();

    assert_eq!(headers[header::USER_AGENT], "OpenAI/JS 4.76.0");
}

#[test]
fn reserved_upstream_headers_are_rejected() {
    for name in [
        "Authorization",
        "Content-Type",
        "Accept",
        "Content-Length",
        "Host",
    ] {
        let config = AdapterConfig {
            base_url: "https://provider.test/v1".to_owned(),
            api_key: Some("secret".to_owned()),
            api_key_env: None,
            upstream_headers: Some(HashMap::from([(name.to_owned(), "wrong".to_owned())])),
            upstream_capabilities: Default::default(),
        };

        let error = build_default_headers(&config).unwrap_err();

        assert!(error.contains("is reserved and cannot be configured"));
    }
}

#[test]
fn invalid_api_key_header_does_not_leak_the_secret() {
    let config = AdapterConfig {
        base_url: "https://provider.test/v1".to_owned(),
        api_key: Some("secret\nvalue".to_owned()),
        api_key_env: None,
        upstream_headers: None,
        upstream_capabilities: Default::default(),
    };

    let error = build_default_headers(&config).unwrap_err();

    assert!(error.contains("Invalid API key header"));
    assert!(!error.contains("secret"));
}

#[test]
fn plain_upstream_error_keeps_status_and_sanitized_request_shape() {
    let request = json!({
        "model": "glm-5.2",
        "messages": [
            {"role": "system", "content": "secret system prompt"},
            {"role": "user", "content": [
                {"type": "text", "text": "secret prompt"},
                {"type": "image_url", "image_url": {"url": "https://secret.test/a.png"}}
            ]}
        ],
        "max_tokens": 128,
        "stream": true,
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_secret",
                "description": "secret description",
                "parameters": {"type": "object", "secret_schema": true}
            }
        }],
        "tool_choice": "auto",
        "thinking": {"type": "enabled"},
        "tool_stream": true
    });
    let shape = upstream_request_shape(&request);
    let serialized = shape.to_string();
    for secret in [
        "secret system prompt",
        "secret prompt",
        "https://secret.test/a.png",
        "lookup_secret",
        "secret description",
        "secret_schema",
    ] {
        assert!(!serialized.contains(secret), "shape leaked {secret}");
    }
    assert_eq!(shape["modelFamily"], "glm-5");
    assert_eq!(shape["maxTokenField"], "max_tokens");
    assert_eq!(shape["maxTokenValue"], 128);
    assert_eq!(shape["messageCount"], 2);
    assert_eq!(shape["thinkingPresent"], true);
    assert_eq!(shape["terminalAssistantPrefill"], false);
    assert_eq!(shape["toolCount"], 1);
    assert_eq!(shape["toolChoiceType"], "auto");
    assert_eq!(shape["messageContentTypes"]["image_url"], 1);
    assert_eq!(
        shape["toolFunctionFields"],
        json!(["description", "name", "parameters"])
    );

    let error = build_upstream_error(
        StatusCode::NOT_ACCEPTABLE,
        "Not Acceptable".to_owned(),
        shape.clone(),
    );
    assert_eq!(error.status, StatusCode::NOT_ACCEPTABLE);
    assert_eq!(error.message, "Not Acceptable");
    assert_eq!(
        error.details.as_ref().unwrap()["response"],
        "Not Acceptable"
    );
    assert_eq!(
        error.details.as_ref().unwrap()["upstreamRequestShape"],
        shape
    );
    let namespaced_qwen = upstream_request_shape(&json!({
        "model": "qwen/qwen3.6-35b-a3b",
        "messages": [{"role": "user", "content": "secret"}],
        "max_tokens": 64,
        "stream": true,
        "enable_thinking": true
    }));
    assert_eq!(namespaced_qwen["modelFamily"], "qwen3");
    assert_eq!(namespaced_qwen["thinkingPresent"], true);
    assert!(!namespaced_qwen.to_string().contains("secret"));

    let terminal_prefill = upstream_request_shape(&json!({
        "model": "model",
        "messages": [
            {"role": "user", "content": "secret user"},
            {"role": "assistant", "content": "secret prefill"}
        ]
    }));
    assert_eq!(terminal_prefill["terminalAssistantPrefill"], true);
    assert!(!terminal_prefill.to_string().contains("secret"));
}

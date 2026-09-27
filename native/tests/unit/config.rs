use super::*;

fn config(api_key: Option<&str>, api_key_env: Option<&str>) -> AdapterConfig {
    AdapterConfig {
        base_url: "https://provider.test/v1".to_owned(),
        api_key: api_key.map(str::to_owned),
        api_key_env: api_key_env.map(str::to_owned),
        upstream_headers: None,
        upstream_capabilities: UpstreamCapabilities::default(),
    }
}

#[test]
fn defaults_assistant_prefill_to_unsupported() {
    let config: AdapterConfig = serde_json::from_value(serde_json::json!({
        "baseUrl": "https://provider.test/v1",
        "apiKey": "secret"
    }))
    .unwrap();

    assert_eq!(
        config.upstream_capabilities.assistant_prefill,
        AssistantPrefillMode::Unsupported
    );
}

#[test]
fn parses_supported_assistant_prefill_modes() {
    for (value, expected) in [
        ("unsupported", AssistantPrefillMode::Unsupported),
        (
            "continue_final_message",
            AssistantPrefillMode::ContinueFinalMessage,
        ),
        ("native", AssistantPrefillMode::Native),
    ] {
        let config: AdapterConfig = serde_json::from_value(serde_json::json!({
            "baseUrl": "https://provider.test/v1",
            "apiKey": "secret",
            "upstreamCapabilities": {"assistantPrefill": value}
        }))
        .unwrap();
        assert_eq!(config.upstream_capabilities.assistant_prefill, expected);
    }
}

#[test]
fn rejects_unknown_assistant_prefill_mode() {
    let error = serde_json::from_value::<AdapterConfig>(serde_json::json!({
        "baseUrl": "https://provider.test/v1",
        "apiKey": "secret",
        "upstreamCapabilities": {"assistantPrefill": "guess"}
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown variant"));
}

#[test]
fn resolves_plaintext_api_key() {
    assert_eq!(
        config(Some("secret"), None)
            .resolve_api_key_with(|_| unreachable!())
            .unwrap(),
        "secret"
    );
}

#[test]
fn resolves_api_key_from_environment() {
    let resolved = config(None, Some("OPENAI_API_KEY"))
        .resolve_api_key_with(|name| {
            assert_eq!(name, "OPENAI_API_KEY");
            Ok("secret".to_owned())
        })
        .unwrap();
    assert_eq!(resolved, "secret");
}

#[test]
fn rejects_ambiguous_or_missing_credentials() {
    for configured in [config(Some("secret"), Some("KEY")), config(None, None)] {
        assert!(
            configured
                .resolve_api_key_with(|_| unreachable!())
                .unwrap_err()
                .contains("exactly one")
        );
    }
}

#[test]
fn rejects_invalid_missing_and_empty_environment_variables() {
    assert!(
        config(None, Some("INVALID-NAME"))
            .resolve_api_key_with(|_| unreachable!())
            .unwrap_err()
            .contains("must match")
    );
    assert!(
        config(None, Some("MISSING_KEY"))
            .resolve_api_key_with(|_| Err(env::VarError::NotPresent))
            .unwrap_err()
            .contains("is not set")
    );
    assert!(
        config(None, Some("EMPTY_KEY"))
            .resolve_api_key_with(|_| Ok(String::new()))
            .unwrap_err()
            .contains("is empty")
    );
}

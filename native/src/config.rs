use std::{collections::HashMap, env, path::Path};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub upstream_headers: Option<HashMap<String, String>>,
}

impl AdapterConfig {
    pub async fn load(path: &Path) -> Result<Self, String> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|error| format!("Failed to read config {}: {error}", path.display()))?;
        serde_json::from_str(&content)
            .map_err(|error| format!("Invalid config {}: {error}", path.display()))
    }

    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    pub fn resolve_api_key(&self) -> Result<String, String> {
        self.resolve_api_key_with(|name| env::var(name))
    }

    fn resolve_api_key_with(
        &self,
        lookup: impl FnOnce(&str) -> Result<String, env::VarError>,
    ) -> Result<String, String> {
        match (&self.api_key, &self.api_key_env) {
            (Some(_), Some(_)) => {
                Err("Configuration must contain exactly one of apiKey or apiKeyEnv".to_owned())
            }
            (None, None) => {
                Err("Configuration must contain exactly one of apiKey or apiKeyEnv".to_owned())
            }
            (Some(api_key), None) if api_key.is_empty() => {
                Err("Configuration apiKey must not be empty".to_owned())
            }
            (Some(api_key), None) => Ok(api_key.clone()),
            (None, Some(name)) if !is_portable_env_name(name) => Err(format!(
                "Configuration apiKeyEnv {name:?} must match [A-Za-z_][A-Za-z0-9_]*"
            )),
            (None, Some(name)) => match lookup(name) {
                Ok(value) if value.is_empty() => Err(format!(
                    "Environment variable {name} configured by apiKeyEnv is empty"
                )),
                Ok(value) => Ok(value),
                Err(env::VarError::NotPresent) => Err(format!(
                    "Environment variable {name} configured by apiKeyEnv is not set; set it and restart claude-adapter"
                )),
                Err(env::VarError::NotUnicode(_)) => Err(format!(
                    "Environment variable {name} configured by apiKeyEnv is not valid Unicode"
                )),
            },
        }
    }
}

fn is_portable_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(api_key: Option<&str>, api_key_env: Option<&str>) -> AdapterConfig {
        AdapterConfig {
            base_url: "https://provider.test/v1".to_owned(),
            api_key: api_key.map(str::to_owned),
            api_key_env: api_key_env.map(str::to_owned),
            upstream_headers: None,
        }
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
}

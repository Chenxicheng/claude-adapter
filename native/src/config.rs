use std::{collections::HashMap, path::Path};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterConfig {
    pub base_url: String,
    pub api_key: String,
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
}

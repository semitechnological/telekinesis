//! Host MCP server config: ~/.telekinesis/mcp.json
//!
//! Engine owns stdio transport. Host loads config and connects best-effort.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// `stdio` (supported), `http` / `sse` (documented, not wired in host yet).
    #[serde(default = "default_transport")]
    pub transport: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub url: Option<String>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

#[derive(Debug, Deserialize, Default)]
struct McpConfigFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".telekinesis/mcp.json"))
        .unwrap_or_else(|| PathBuf::from(".telekinesis/mcp.json"))
}

/// Load MCP server entries. Missing/invalid file → empty list (TUI still starts).
pub fn load() -> Vec<McpServerConfig> {
    let path = config_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<McpConfigFile>(&raw) {
        Ok(file) => file.servers,
        Err(e) => {
            eprintln!("telekinesis: ignore invalid {}: {e}", path.display());
            Vec::new()
        }
    }
}

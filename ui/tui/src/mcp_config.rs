//! Host MCP server config: ~/.telekinesis/mcp.json
//!
//! Engine owns stdio + remote HTTP/SSE transports. Host loads config and connects best-effort.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
struct McpConfigFile {
    #[serde(default)]
    servers: Vec<rx4::McpServerConfig>,
}

pub fn config_path() -> Option<PathBuf> {
    crate::host::config_home().map(|home| home.join("mcp.json"))
}

pub fn load() -> Vec<rx4::McpServerConfig> {
    let Some(path) = config_path() else {
        return Vec::new();
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_home_loads_no_mcp() {
        if dirs::home_dir().is_none() {
            assert!(config_path().is_none());
            assert!(load().is_empty());
        } else {
            assert_eq!(
                config_path(),
                Some(dirs::home_dir().unwrap().join(".telekinesis/mcp.json"))
            );
        }
    }
}

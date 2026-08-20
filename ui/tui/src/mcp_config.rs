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
    config_path_from(dirs::home_dir())
}

pub(crate) fn config_path_from(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|home| home.join(".telekinesis").join("mcp.json"))
}

pub fn load() -> Vec<rx4::McpServerConfig> {
    load_from(config_path())
}

pub(crate) fn load_from(path: Option<PathBuf>) -> Vec<rx4::McpServerConfig> {
    let Some(path) = path else {
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
        assert!(config_path_from(None).is_none());
        assert!(load_from(None).is_empty());
        assert!(
            config_path().is_none() || config_path().is_some_and(|path| path.is_absolute()),
            "MCP config must not resolve to a cwd-relative ./.telekinesis/mcp.json"
        );
        if dirs::home_dir().is_some() {
            assert_eq!(
                config_path(),
                Some(dirs::home_dir().unwrap().join(".telekinesis/mcp.json"))
            );
        }
    }

    #[test]
    fn repo_dot_telekinesis_mcp_is_never_user_config() {
        let dir = tempfile::tempdir().unwrap();
        let planted = dir.path().join(".telekinesis");
        std::fs::create_dir_all(&planted).unwrap();
        let planted_file = planted.join("mcp.json");
        std::fs::write(
            &planted_file,
            r#"{"servers":[{"name":"cwd-must-not-spawn","command":"python3","args":["-c","pass"]}]}"#,
        )
        .unwrap();
        assert!(load_from(None).is_empty());
        assert!(load_from(config_path_from(None)).is_empty());
        assert!(load()
            .iter()
            .all(|server| server.name != "cwd-must-not-spawn"));
        let loaded = load_from(Some(planted_file));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "cwd-must-not-spawn");
        assert!(config_path()
            .map(|path| path != PathBuf::from(".telekinesis/mcp.json"))
            .unwrap_or(true));
    }
}

//! Disk cache of MCP tool schemas: ~/.telekinesis/mcp-cache.json
//!
//! Tool lists discovered on previous runs are advertised again at the next
//! startup without waiting on the network, so the model sees a stable tool
//! set from the first turn (pre-advertisement). Entries are keyed by server
//! name plus a hash of the server's config, so editing a server's command,
//! args, url, or headers invalidates its entry. A corrupt or unreadable
//! cache is treated as empty — it must never break startup.

use crate::mcp_config::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One tool as advertised by a server: exactly what registration needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON input schema, stored verbatim as a string.
    pub input_schema: String,
}

/// A server's cached tool list, valid only while `config_hash` matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedServer {
    pub config_hash: String,
    #[serde(default)]
    pub tools: Vec<CachedTool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCache {
    #[serde(default)]
    pub servers: BTreeMap<String, CachedServer>,
}

pub fn cache_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".telekinesis/mcp-cache.json"))
        .unwrap_or_else(|| PathBuf::from(".telekinesis/mcp-cache.json"))
}

/// Load the cache. Missing, unreadable, or corrupt file → empty cache.
pub fn load() -> McpCache {
    load_from(&cache_path())
}

fn load_from(path: &Path) -> McpCache {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return McpCache::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Persist the cache best-effort; a failed write only costs the next startup
/// its head start, so errors are ignored.
pub fn save(cache: &McpCache) {
    let _ = save_to(&cache_path(), cache);
}

fn save_to(path: &Path, cache: &McpCache) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cache).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Fingerprint of everything that affects what a server can advertise.
/// Any edit to the server's transport, command, args, url, or headers
/// produces a different hash and therefore invalidates its cache entry.
pub fn config_hash(cfg: &McpServerConfig) -> String {
    let mut hasher = Fnv1a::default();
    for part in [
        cfg.name.as_str(),
        cfg.transport.as_str(),
        cfg.command.as_deref().unwrap_or_default(),
        cfg.url.as_deref().unwrap_or_default(),
    ] {
        hasher.write(part);
    }
    for arg in &cfg.args {
        hasher.write(arg);
    }
    let mut headers: Vec<_> = cfg.headers.iter().collect();
    headers.sort();
    for (key, value) in headers {
        hasher.write(key);
        hasher.write(value);
    }
    format!("{:016x}", hasher.finish())
}

/// The server's cached entry, but only if its config has not changed.
pub fn lookup<'a>(cache: &'a McpCache, cfg: &McpServerConfig) -> Option<&'a CachedServer> {
    cache
        .servers
        .get(&cfg.name)
        .filter(|entry| entry.config_hash == config_hash(cfg))
}

/// FNV-1a, 64-bit. `DefaultHasher` is not guaranteed stable across Rust
/// releases, and this hash lands on disk, so we pin the algorithm.
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv1a {
    fn write(&mut self, part: &str) {
        // Length-prefix each part so ["ab","c"] and ["a","bc"] differ.
        for byte in (part.len() as u64)
            .to_le_bytes()
            .iter()
            .chain(part.as_bytes())
        {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some("mcp-server".to_string()),
            args: vec!["--flag".to_string()],
            url: None,
            headers: Default::default(),
        }
    }

    fn tool(name: &str) -> CachedTool {
        CachedTool {
            name: name.to_string(),
            description: format!("{name} description"),
            input_schema: r#"{"type":"object"}"#.to_string(),
        }
    }

    #[test]
    fn cache_roundtrips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/mcp-cache.json");
        let mut cache = McpCache::default();
        cache.servers.insert(
            "files".to_string(),
            CachedServer {
                config_hash: config_hash(&server("files")),
                tools: vec![tool("read"), tool("write")],
            },
        );

        save_to(&path, &cache).unwrap();
        assert_eq!(load_from(&path), cache);
    }

    #[test]
    fn missing_or_corrupt_cache_is_empty_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-cache.json");
        assert_eq!(load_from(&path), McpCache::default());

        std::fs::write(&path, "{ not json ]").unwrap();
        assert_eq!(load_from(&path), McpCache::default());

        std::fs::write(&path, r#"{"servers": 7}"#).unwrap();
        assert_eq!(load_from(&path), McpCache::default());
    }

    #[test]
    fn config_hash_is_stable_and_sensitive_to_every_field() {
        let base = server("files");
        assert_eq!(config_hash(&base), config_hash(&server("files")));

        let mut command = base.clone();
        command.command = Some("other".to_string());
        let mut args = base.clone();
        args.args.push("--extra".to_string());
        let mut url = base.clone();
        url.url = Some("http://localhost:9".to_string());
        let mut transport = base.clone();
        transport.transport = "sse".to_string();
        let mut headers = base.clone();
        headers
            .headers
            .insert("authorization".to_string(), "token".to_string());

        for changed in [&command, &args, &url, &transport, &headers] {
            assert_ne!(config_hash(&base), config_hash(changed));
        }
    }

    #[test]
    fn lookup_requires_matching_config_hash() {
        let mut cache = McpCache::default();
        cache.servers.insert(
            "files".to_string(),
            CachedServer {
                config_hash: config_hash(&server("files")),
                tools: vec![tool("read")],
            },
        );

        assert!(lookup(&cache, &server("files")).is_some());
        assert!(lookup(&cache, &server("other")).is_none());

        let mut edited = server("files");
        edited.args.push("--changed".to_string());
        assert!(lookup(&cache, &edited).is_none(), "config edit invalidates");
    }
}

//! Pi JSONL v3 session format — entry types, header, persistence.
//!
//! Compatible with pi_agent_rust session format:
//! - Location: ~/.pi/agent/sessions/--encoded-project-path--/
//! - Filename: YYYY-MM-DDTHH-MM-SS.sssZ_id.jsonl
//! - Format: JSON Lines (header + entries)

use chrono::{DateTime, Utc};
use rx4::provider::{Message, Role};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Custom-entry extension name for tool calls appended by the TUI.
pub const TOOL_CALL_EXTENSION: &str = "telekinesis.tool_call";
/// Custom-entry extension name for tool results appended by the TUI.
pub const TOOL_RESULT_EXTENSION: &str = "telekinesis.tool_result";
/// Durable `session_info` key written before a persist rewrite so a truncated
/// tail is not lost.
pub const INTERRUPTED_INFO_KEY: &str = "interrupted";
/// Esc-cancel `session_info` key so a cancelled turn is not classified as a crash.
pub const CANCELLED_INFO_KEY: &str = "cancelled";

/// Session header — first line of the JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiSessionHeader {
    pub version: u32,
    pub id: String,
    pub project: String,
    pub created: DateTime<Utc>,
    pub model: String,
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PiSessionHeader {
    pub fn new(project: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            version: crate::pi::PI_SESSION_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            project: project.into(),
            created: Utc::now(),
            model: model.into(),
            provider: None,
            label: None,
        }
    }
}

/// Entry types in a pi session (pi_agent_rust SessionEntry).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiEntryType {
    #[serde(rename = "message")]
    Message {
        role: Role,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
    #[serde(rename = "model_change")]
    ModelChange { from: String, to: String },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange { level: String },
    #[serde(rename = "compaction")]
    Compaction { summary: String, cut_at: usize },
    #[serde(rename = "branch_summary")]
    BranchSummary { from_session: String, at_entry: u64 },
    #[serde(rename = "session_info")]
    SessionInfo { key: String, value: String },
    #[serde(rename = "label")]
    Label { text: String },
    #[serde(rename = "custom")]
    Custom {
        extension: String,
        payload: serde_json::Value,
    },
}

/// A single entry in the session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiEntry {
    #[serde(flatten)]
    pub entry_type: PiEntryType,
    pub timestamp: DateTime<Utc>,
    pub id: u64,
    pub parent_id: Option<u64>,
}

/// Pi-format session — JSONL v3 with typed entries and tree structure.
pub struct PiSession {
    pub header: PiSessionHeader,
    pub entries: Vec<PiEntry>,
    next_id: u64,
    persisted_entries: usize,
    persisted_bytes: u64,
}

impl PiSession {
    pub fn new(project: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            header: PiSessionHeader::new(project, model),
            entries: Vec::new(),
            next_id: 1,
            persisted_entries: 0,
            persisted_bytes: 0,
        }
    }

    pub fn append(&mut self, entry_type: PiEntryType) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let parent = self.entries.last().map(|e| e.id);
        self.entries.push(PiEntry {
            entry_type,
            timestamp: Utc::now(),
            id,
            parent_id: parent,
        });
        id
    }

    pub fn append_message(&mut self, role: Role, content: impl Into<String>) -> u64 {
        self.append(PiEntryType::Message {
            role,
            content: content.into(),
            tool_call_id: None,
        })
    }

    pub fn append_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
    ) -> u64 {
        self.append(PiEntryType::Message {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
        })
    }

    pub fn append_model_change(&mut self, from: impl Into<String>, to: impl Into<String>) -> u64 {
        self.append(PiEntryType::ModelChange {
            from: from.into(),
            to: to.into(),
        })
    }

    pub fn append_compaction(&mut self, summary: impl Into<String>, cut_at: usize) -> u64 {
        self.append(PiEntryType::Compaction {
            summary: summary.into(),
            cut_at,
        })
    }

    pub fn append_label(&mut self, text: impl Into<String>) -> u64 {
        self.append(PiEntryType::Label { text: text.into() })
    }

    /// Fork the session from a specific entry (pi branching).
    pub fn fork(&self, from_entry: u64) -> Self {
        let mut forked = Self::new(self.header.project.clone(), self.header.model.clone());
        forked.header.id = uuid::Uuid::new_v4().to_string();
        forked.header.label = Some(format!("fork of {} at {}", self.header.id, from_entry));

        for entry in &self.entries {
            forked.entries.push(PiEntry {
                entry_type: clone_entry_type(&entry.entry_type),
                timestamp: entry.timestamp,
                id: entry.id,
                parent_id: entry.parent_id,
            });
            if entry.id == from_entry {
                break;
            }
        }
        forked.next_id = self.next_id;
        forked
    }

    /// Path this session persists to.
    pub fn jsonl_path(&self, dir: &Path) -> std::path::PathBuf {
        dir.join(format!(
            "{}_{}.jsonl",
            self.header.created.format("%Y-%m-%dT%H-%M-%S%.3fZ"),
            &self.header.id[..8]
        ))
    }

    /// Save as JSONL v3 (header on first line, entries follow).
    ///
    /// Appends only the entries written since the last successful save. Falls
    /// back to a full atomic rewrite when the file is missing, was truncated or
    /// rewritten behind our back, or the watermark no longer matches.
    pub fn save_jsonl(&mut self, dir: &Path) -> std::io::Result<std::path::PathBuf> {
        use std::io::Write;

        std::fs::create_dir_all(dir)?;
        let path = self.jsonl_path(dir);

        let appendable = self.persisted_entries <= self.entries.len()
            && self.persisted_bytes > 0
            && std::fs::metadata(&path)
                .map(|m| m.len() == self.persisted_bytes)
                .unwrap_or(false);

        if appendable {
            let mut added = String::new();
            for entry in &self.entries[self.persisted_entries..] {
                added.push_str(&serde_json::to_string(entry).unwrap());
                added.push('\n');
            }
            if added.is_empty() {
                return Ok(path);
            }
            let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
            file.write_all(added.as_bytes())?;
            file.sync_all()?;
            self.persisted_entries = self.entries.len();
            self.persisted_bytes += added.len() as u64;
            return Ok(path);
        }

        let mut content = String::new();
        content.push_str(&serde_json::to_string(&self.header).unwrap());
        content.push('\n');
        for entry in &self.entries {
            content.push_str(&serde_json::to_string(entry).unwrap());
            content.push('\n');
        }
        let temporary = path.with_extension("jsonl.tmp");
        {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(temporary, &path)?;
        self.persisted_entries = self.entries.len();
        self.persisted_bytes = content.len() as u64;
        Ok(path)
    }

    /// Load a JSONL v3 session file.
    pub fn load_jsonl(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut lines = content.lines();
        let header_line = lines.next().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "empty session file")
        })?;
        let header: PiSessionHeader = serde_json::from_str(header_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut entries = Vec::new();
        let mut next_id = 1u64;
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<PiEntry>(line) {
                if entry.id >= next_id {
                    next_id = entry.id + 1;
                }
                entries.push(entry);
            }
        }

        Ok(Self {
            header,
            entries,
            next_id,
            persisted_entries: 0,
            persisted_bytes: 0,
        })
    }

    /// Convert entries to provider Messages for the agent loop.
    pub fn messages(&self) -> Vec<Message> {
        self.entries
            .iter()
            .filter_map(|e| match &e.entry_type {
                PiEntryType::Message {
                    role,
                    content,
                    tool_call_id,
                } => {
                    if let Some(tid) = tool_call_id {
                        Some(Message::tool(tid, content.clone()))
                    } else {
                        Some(Message::new(*role, content.clone()))
                    }
                }
                _ => None,
            })
            .collect()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.entry_type, PiEntryType::Message { .. }))
            .count()
    }
}

/// Evidence that a session file ended mid-turn (crash, kill, closed terminal).
#[derive(Debug, Clone)]
pub struct InterruptInfo {
    /// Timestamp of the last activity that made it to disk.
    ///
    /// For a truncated trailing line this is the file mtime, not the last
    /// parsed entry — a crash mid-append can leave parsed rows hours old.
    pub last_activity: DateTime<Utc>,
    /// The last user prompt in the session (full text; excerpt at display).
    pub last_prompt: String,
    /// Parsed `message` entries in the file.
    pub message_count: usize,
    /// All parsed entries in the file.
    pub entry_count: usize,
}

fn entry_in_flight(entry: &PiEntryType) -> Option<bool> {
    match entry {
        PiEntryType::Message {
            tool_call_id: Some(_),
            ..
        } => Some(true),
        PiEntryType::Message { role, .. } => match role {
            Role::Assistant => Some(false),
            Role::User | Role::Tool => Some(true),
            Role::System => None,
        },
        PiEntryType::Custom { extension, .. }
            if extension == TOOL_CALL_EXTENSION || extension == TOOL_RESULT_EXTENSION =>
        {
            Some(true)
        }
        PiEntryType::Compaction { .. } => Some(true),
        PiEntryType::SessionInfo { key, .. } if key == INTERRUPTED_INFO_KEY => Some(true),
        PiEntryType::SessionInfo { key, .. } if key == CANCELLED_INFO_KEY => Some(false),
        _ => None,
    }
}

fn file_mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

/// Detect whether a session file was interrupted mid-turn.
///
/// Returns `Some(InterruptInfo)` when the trailing entries show a turn still
/// in flight, the file ends in an unparseable JSONL line, or a durable
/// `session_info` interrupted marker is present. Returns `None` for
/// cleanly-ended sessions, Esc-cancelled sessions marked as such, sessions
/// without any user prompt, and files whose header is missing or corrupt.
/// Never panics on truncated or garbage input.
pub fn session_interrupted(path: &Path) -> Option<InterruptInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let header_line = lines.next()?;
    serde_json::from_str::<PiSessionHeader>(header_line).ok()?;

    let mut entries: Vec<PiEntry> = Vec::new();
    let mut corrupt_tail = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<PiEntry>(line) {
            Ok(entry) => {
                entries.push(entry);
                corrupt_tail = false;
            }
            Err(_) => corrupt_tail = true,
        }
    }

    let last_prompt = entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.entry_type {
            PiEntryType::Message {
                role: Role::User,
                content,
                tool_call_id: None,
            } => Some(content.clone()),
            _ => None,
        })?;

    let in_flight = entries
        .iter()
        .rev()
        .find_map(|entry| entry_in_flight(&entry.entry_type));
    if !corrupt_tail && in_flight != Some(true) {
        return None;
    }

    let last_activity = if corrupt_tail {
        file_mtime_utc(path).or_else(|| entries.last().map(|entry| entry.timestamp))?
    } else {
        entries.last().map(|entry| entry.timestamp)?
    };

    Some(InterruptInfo {
        last_activity,
        last_prompt,
        message_count: entries
            .iter()
            .filter(|entry| matches!(entry.entry_type, PiEntryType::Message { .. }))
            .count(),
        entry_count: entries.len(),
    })
}

fn clone_entry_type(et: &PiEntryType) -> PiEntryType {
    match et {
        PiEntryType::Message {
            role,
            content,
            tool_call_id,
        } => PiEntryType::Message {
            role: *role,
            content: content.clone(),
            tool_call_id: tool_call_id.clone(),
        },
        PiEntryType::ModelChange { from, to } => PiEntryType::ModelChange {
            from: from.clone(),
            to: to.clone(),
        },
        PiEntryType::ThinkingLevelChange { level } => PiEntryType::ThinkingLevelChange {
            level: level.clone(),
        },
        PiEntryType::Compaction { summary, cut_at } => PiEntryType::Compaction {
            summary: summary.clone(),
            cut_at: *cut_at,
        },
        PiEntryType::BranchSummary {
            from_session,
            at_entry,
        } => PiEntryType::BranchSummary {
            from_session: from_session.clone(),
            at_entry: *at_entry,
        },
        PiEntryType::SessionInfo { key, value } => PiEntryType::SessionInfo {
            key: key.clone(),
            value: value.clone(),
        },
        PiEntryType::Label { text } => PiEntryType::Label { text: text.clone() },
        PiEntryType::Custom { extension, payload } => PiEntryType::Custom {
            extension: extension.clone(),
            payload: payload.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn session_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test/project", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi there");
        s.append_label("test-label");

        let path = s.save_jsonl(tmp.path()).unwrap();
        let loaded = PiSession::load_jsonl(&path).unwrap();
        assert_eq!(loaded.header.model, "gpt-5.5");
        assert_eq!(loaded.entry_count(), 3);
        assert_eq!(loaded.message_count(), 2);
    }

    #[test]
    fn incremental_appends_accumulate() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test/project", "gpt-5.5");
        s.append_message(Role::User, "one");
        let path = s.save_jsonl(tmp.path()).unwrap();

        s.append_message(Role::Assistant, "two");
        assert_eq!(s.save_jsonl(tmp.path()).unwrap(), path);
        s.append_message(Role::User, "three");
        assert_eq!(s.save_jsonl(tmp.path()).unwrap(), path);

        let loaded = PiSession::load_jsonl(&path).unwrap();
        assert_eq!(loaded.entry_count(), 3);
        let contents: Vec<String> = loaded
            .messages()
            .iter()
            .map(|m| m.content.clone())
            .collect();
        assert_eq!(contents, vec!["one", "two", "three"]);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            loaded_bytes(&path),
            "file must not carry stale bytes"
        );
    }

    #[test]
    fn stale_watermark_falls_back_to_full_rewrite() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test/project", "gpt-5.5");
        s.append_message(Role::User, "one");
        let path = s.save_jsonl(tmp.path()).unwrap();

        // Simulate a crash mid-write: the file no longer matches the watermark.
        let truncated = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &truncated[..truncated.len() / 2]).unwrap();

        s.append_message(Role::Assistant, "two");
        s.save_jsonl(tmp.path()).unwrap();

        let loaded = PiSession::load_jsonl(&path).unwrap();
        assert_eq!(loaded.entry_count(), 2);
        assert_eq!(loaded.message_count(), 2);
    }

    fn loaded_bytes(path: &Path) -> u64 {
        std::fs::read_to_string(path).unwrap().len() as u64
    }

    #[test]
    fn fork_preserves_prefix() {
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "first");
        let fork_point = s.append_message(Role::Assistant, "second");
        s.append_message(Role::User, "third");

        let forked = s.fork(fork_point);
        assert_eq!(forked.entry_count(), 2);
    }

    #[test]
    fn messages_extracts_only_messages() {
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_model_change("gpt-5.5", "gpt-5.4-mini");
        s.append_message(Role::Assistant, "hi");

        let msgs = s.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    fn saved(session: &mut PiSession, dir: &TempDir) -> std::path::PathBuf {
        session.save_jsonl(dir.path()).unwrap()
    }

    #[test]
    fn clean_session_is_not_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "fix the tests");
        s.append_message(Role::Assistant, "done, all green");
        let path = saved(&mut s, &tmp);
        assert!(session_interrupted(&path).is_none());
    }

    #[test]
    fn trailing_user_prompt_is_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "first ask");
        s.append_message(Role::Assistant, "first answer");
        s.append_message(Role::User, "second ask, never answered");
        let path = saved(&mut s, &tmp);
        let info = session_interrupted(&path).expect("prompt without reply");
        assert_eq!(info.last_prompt, "second ask, never answered");
        assert_eq!(info.message_count, 3);
        assert_eq!(info.entry_count, 3);
    }

    #[test]
    fn trailing_tool_call_is_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "run the build");
        s.append_message(Role::Assistant, "running it");
        s.append(PiEntryType::Custom {
            extension: TOOL_CALL_EXTENSION.to_string(),
            payload: serde_json::json!({"id": "1", "name": "bash", "arguments": "{}"}),
        });
        let path = saved(&mut s, &tmp);
        let info = session_interrupted(&path).expect("tool call without result");
        assert_eq!(info.last_prompt, "run the build");
    }

    #[test]
    fn trailing_tool_result_is_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "run the build");
        s.append_tool_result("1", "compiled");
        let path = saved(&mut s, &tmp);
        let info = session_interrupted(&path).expect("tool result awaiting model");
        assert_eq!(info.last_prompt, "run the build");
    }

    #[test]
    fn trailing_metadata_does_not_mask_clean_end() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi");
        s.append_model_change("gpt-5.5", "gpt-5.4-mini");
        let path = saved(&mut s, &tmp);
        assert!(session_interrupted(&path).is_none());
    }

    #[test]
    fn truncated_tail_is_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi");
        let path = saved(&mut s, &tmp);

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"message\",\"role\":\"assis")
            .unwrap();
        drop(file);

        let info = session_interrupted(&path).expect("truncated tail");
        assert_eq!(info.last_prompt, "hello");
        assert_eq!(info.entry_count, 2, "partial line must not count");
    }

    #[test]
    fn truncated_tail_uses_file_mtime_not_parsed_timestamp() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi");
        let old = Utc::now() - chrono::Duration::hours(48);
        for entry in &mut s.entries {
            entry.timestamp = old;
        }
        let path = saved(&mut s, &tmp);

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"message\",\"role\":\"assis")
            .unwrap();
        file.sync_all().unwrap();
        drop(file);

        let info = session_interrupted(&path).expect("truncated tail");
        let age = Utc::now().signed_duration_since(info.last_activity);
        assert!(
            age < chrono::Duration::minutes(1),
            "corrupt tail must use mtime, got {age:?} (parsed stamp was 48h old)"
        );
    }

    #[test]
    fn mid_file_garbage_then_clean_assistant_is_not_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi");
        let path = saved(&mut s, &tmp);

        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = raw.lines().collect();
        lines.insert(2, "{ not json");
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        assert!(
            session_interrupted(&path).is_none(),
            "garbage that is not the trailing line must not classify the session"
        );
    }

    #[test]
    fn interrupted_marker_survives_clean_looking_rewrite() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi");
        s.append(PiEntryType::SessionInfo {
            key: INTERRUPTED_INFO_KEY.to_string(),
            value: "truncated-tail".to_string(),
        });
        let path = saved(&mut s, &tmp);
        let info = session_interrupted(&path).expect("durable interrupted marker");
        assert_eq!(info.last_prompt, "hello");
    }

    #[test]
    fn cancelled_marker_is_not_interrupted() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test", "gpt-5.5");
        s.append_message(Role::User, "hello");
        s.append(PiEntryType::SessionInfo {
            key: CANCELLED_INFO_KEY.to_string(),
            value: "esc".to_string(),
        });
        let path = saved(&mut s, &tmp);
        assert!(
            session_interrupted(&path).is_none(),
            "Esc-cancel must not look like a crash"
        );
    }

    #[test]
    fn sessions_without_prompt_or_header_are_ignored() {
        let tmp = TempDir::new().unwrap();

        let mut empty = PiSession::new("/test", "gpt-5.5");
        let path = saved(&mut empty, &tmp);
        assert!(session_interrupted(&path).is_none());

        let garbage = tmp.path().join("garbage.jsonl");
        std::fs::write(&garbage, "not json at all\n").unwrap();
        assert!(session_interrupted(&garbage).is_none());

        let empty_file = tmp.path().join("empty.jsonl");
        std::fs::write(&empty_file, "").unwrap();
        assert!(session_interrupted(&empty_file).is_none());
    }
}

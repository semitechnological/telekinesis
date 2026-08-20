//! Pi protocol compatibility layer.
//!
//! Product surface: JSONL v3 sessions (`PiSession`) and the embed SDK
//! (`create_agent_session`). Dead RPC/extension/store surfaces were removed.

pub mod sdk;
pub mod session;
pub mod tools;

pub use sdk::{create_agent_session, AgentSessionHandle, AgentSessionOptions, SessionTransport};
pub use session::{
    session_interrupted, InterruptInfo, PiEntry, PiEntryType, PiSession, PiSessionHeader,
    CANCELLED_INFO_KEY, INTERRUPTED_INFO_KEY, TOOL_CALL_EXTENSION, TOOL_RESULT_EXTENSION,
};
pub use tools::{is_pi_tool_name, pi_to_rx4_tool, pi_tool_names, rx4_to_pi_tool};

pub const PI_SESSION_VERSION: u32 = 3;

pub fn pi_data_dir() -> std::path::PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".pi").join("agent");
    }
    std::env::temp_dir().join("telekinesis-pi").join("agent")
}

pub fn pi_sessions_dir(project_path: &std::path::Path) -> std::path::PathBuf {
    let encoded = encode_project_path(project_path);
    pi_data_dir().join("sessions").join(encoded)
}

pub fn encode_project_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.replace(['/', '\\'], "--")
        .replace(' ', "-")
        .replace(':', "")
}

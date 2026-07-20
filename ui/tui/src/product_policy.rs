//! Host product policy defaults for telekinesis (pi coding-agent layer).
//! Engine (rx4) only matches; this module fills host-owned shell lists.

use rx4::Policy;

/// Tele default coding policy: workspace write + OS sandbox + safe shell allows.
pub fn tele_coding_policy() -> Policy {
    Policy::workspace_write()
        .with_os_sandbox(true)
        .with_shell_allow([
            "git *",
            "cargo test*",
            "cargo check*",
            "cargo build*",
            "cargo clippy*",
            "cargo fmt*",
            "rg *",
            "fd *",
            "ls *",
            "pwd",
            "cat *",
            "head *",
            "tail *",
            "wc *",
        ])
        .with_shell_deny(["sudo *", "rm -rf /*", "rm -rf /"])
}

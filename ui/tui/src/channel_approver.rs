//! Sync host Approver: blocks tool gate until UI sends Allow/Deny.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc,
};

use rx4::agent::ToolCall;
use rx4::permissions::{Approver, Decision};

pub struct PendingApproval {
    pub tool_name: String,
    pub arguments: String,
    pub respond: SyncSender<Decision>,
}

#[derive(Clone)]
pub struct ApprovalMode {
    bypass: Arc<AtomicBool>,
}

impl ApprovalMode {
    pub fn is_bypass(&self) -> bool {
        self.bypass.load(Ordering::Acquire)
    }

    pub fn toggle(&self) -> bool {
        !self.bypass.fetch_xor(true, Ordering::AcqRel)
    }
}

pub struct ChannelApprover {
    tx: SyncSender<PendingApproval>,
    mode: ApprovalMode,
}

impl ChannelApprover {
    pub fn pair() -> (Self, Receiver<PendingApproval>) {
        let (tx, rx) = mpsc::sync_channel(8);
        let mode = ApprovalMode {
            bypass: Arc::new(AtomicBool::new(true)),
        };
        (Self { tx, mode }, rx)
    }

    pub fn mode(&self) -> ApprovalMode {
        self.mode.clone()
    }
}

impl Approver for ChannelApprover {
    fn approve(&self, tool_call: &ToolCall) -> Decision {
        if self.mode.is_bypass() {
            return Decision::Allow;
        }
        let (resp_tx, resp_rx) = mpsc::sync_channel(1);
        let pending = PendingApproval {
            tool_name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
            respond: resp_tx,
        };
        if self.tx.send(pending).is_err() {
            return Decision::Deny;
        }
        resp_rx.recv().unwrap_or(Decision::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
        }
    }

    #[test]
    fn bypasses_by_default_and_toggles_to_ask() {
        let (approver, pending) = ChannelApprover::pair();
        let mode = approver.mode();

        assert_eq!(approver.approve(&call()), Decision::Allow);
        assert!(!mode.toggle());

        let worker = std::thread::spawn(move || approver.approve(&call()));
        let request = pending.recv().unwrap();
        request.respond.send(Decision::Deny).unwrap();
        assert_eq!(worker.join().unwrap(), Decision::Deny);
        assert!(mode.toggle());
    }
}

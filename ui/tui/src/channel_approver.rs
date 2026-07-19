//! Sync host Approver: blocks tool gate until UI sends Allow/Deny.

use std::sync::mpsc::{self, Receiver, SyncSender};

use rx4::agent::ToolCall;
use rx4::permissions::{Approver, Decision};

pub struct PendingApproval {
    pub tool_name: String,
    pub arguments: String,
    pub respond: SyncSender<Decision>,
}

pub struct ChannelApprover {
    tx: SyncSender<PendingApproval>,
}

impl ChannelApprover {
    pub fn pair() -> (Self, Receiver<PendingApproval>) {
        let (tx, rx) = mpsc::sync_channel(8);
        (Self { tx }, rx)
    }
}

impl Approver for ChannelApprover {
    fn approve(&self, tool_call: &ToolCall) -> Decision {
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

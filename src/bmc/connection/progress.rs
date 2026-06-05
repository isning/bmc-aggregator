use tokio::sync::mpsc;

use super::state::ConnectionStage;

/// Lightweight channel for backends to report connection progress.
/// The transport's connect_task forwards these as ConnectionEvent.
#[derive(Clone, Debug)]
pub struct ConnectProgress {
    tx: mpsc::UnboundedSender<ConnectionStage>,
}

impl ConnectProgress {
    pub fn report(&self, stage: ConnectionStage) {
        let _ = self.tx.send(stage);
    }

    pub fn new() -> (Self, mpsc::UnboundedReceiver<ConnectionStage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ConnectProgress { tx }, rx)
    }
}

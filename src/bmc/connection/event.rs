use tokio::sync::broadcast;

use super::state::{ConnectionState, ConnectionStage};

#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    StateChanged { feature: String, state: ConnectionState },
    StageChanged { feature: String, stage: ConnectionStage },
    Health { feature: String, online: bool, fps: u32, width: u16, height: u16 },
}

pub type ConnectionEventSender = broadcast::Sender<ConnectionEvent>;
pub type ConnectionEventReceiver = broadcast::Receiver<ConnectionEvent>;

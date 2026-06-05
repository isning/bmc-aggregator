use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

impl ConnectionState {
    pub fn is_connected(self) -> bool {
        matches!(self, ConnectionState::Connected)
    }

    pub fn is_active(self) -> bool {
        !matches!(self, ConnectionState::Disconnected)
    }
}

/// Connection progress stages reported by backends during connect.
#[derive(Debug, Clone, Serialize)]
pub enum ConnectionStage {
    Resolving,
    Connecting,
    Authenticating,
    Handshaking,
    Ready,
    Failed(String),
}

//! Media setup requires both a matching remote status and an authenticated peer.
use core::time::Duration;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Waiting,
    Confirmed,
    Rejected,
    Unknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mount {
    state: State,
    port: u16,
    deadline: Duration,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub port: Option<u16>,
    pub connected: bool,
    pub failed: bool,
    pub peer_ready: bool,
}
impl Mount {
    pub fn new(port: u16, deadline: Duration) -> Self {
        Self {
            state: State::Waiting,
            port,
            deadline,
        }
    }
    pub fn state(self) -> State {
        self.state
    }
    pub fn step(mut self, observation: Observation, now: Duration) -> Self {
        if self.state != State::Waiting {
            return self;
        }
        if now >= self.deadline {
            self.state = State::Unknown;
        } else if observation.port == Some(self.port) {
            if observation.failed {
                self.state = State::Rejected;
            } else if observation.connected && observation.peer_ready {
                self.state = State::Confirmed;
            }
        }
        self
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neither_stale_ports_nor_peer_alone_confirm() {
        let model = Mount::new(123, Duration::from_secs(10));
        let mut event = Observation {
            port: Some(124),
            connected: true,
            failed: false,
            peer_ready: true,
        };
        assert_eq!(model.step(event, Duration::ZERO).state(), State::Waiting);
        event.port = Some(123);
        event.peer_ready = false;
        assert_eq!(model.step(event, Duration::ZERO).state(), State::Waiting);
        event.peer_ready = true;
        assert_eq!(model.step(event, Duration::ZERO).state(), State::Confirmed);
        assert_eq!(
            model.step(event, Duration::from_secs(10)).state(),
            State::Unknown
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attachment {
    Unknown,
    Detached,
    Attached,
    UncertainAttached,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentEvent {
    Connected,
    Disconnected,
    Uncertain,
}
impl Attachment {
    pub fn step(self, event: AttachmentEvent) -> Self {
        match event {
            AttachmentEvent::Connected => Self::Attached,
            AttachmentEvent::Disconnected => Self::Detached,
            AttachmentEvent::Uncertain => {
                if self.connected() {
                    Self::UncertainAttached
                } else {
                    Self::Unknown
                }
            }
        }
    }
    /// Reports for an earlier listener cannot change the current attachment.
    pub fn observe(
        self,
        active_port: Option<u16>,
        reported_port: u16,
        event: AttachmentEvent,
    ) -> Self {
        if active_port.is_some_and(|port| port != reported_port) {
            return self;
        }
        self.step(event)
    }
    pub fn connected(self) -> bool {
        matches!(self, Self::Attached | Self::UncertainAttached)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageShareStatus {
    Connected,
    DisconnectedByRequest,
    ConnectFailed,
    DisconnectError,
    NotConnected,
    Unknown,
}
impl StorageShareStatus {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::Connected,
            1 => Self::DisconnectedByRequest,
            2 => Self::ConnectFailed,
            3 => Self::DisconnectError,
            4 => Self::NotConnected,
            _ => Self::Unknown,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::DisconnectedByRequest => "disconnected_by_request",
            Self::ConnectFailed => "connect_failed",
            Self::DisconnectError => "disconnect_error",
            Self::NotConnected => "not_connected",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn stale_reports_preserve_current_attachment() {
        for event in [
            AttachmentEvent::Connected,
            AttachmentEvent::Disconnected,
            AttachmentEvent::Uncertain,
        ] {
            assert_eq!(
                Attachment::Attached.observe(Some(123), 124, event),
                Attachment::Attached
            );
        }
        assert_eq!(
            Attachment::Attached.observe(Some(123), 123, AttachmentEvent::Disconnected),
            Attachment::Detached
        );
    }
    #[test]
    fn unknown_observations_preserve_possible_ownership() {
        assert_eq!(
            Attachment::Unknown.step(AttachmentEvent::Uncertain),
            Attachment::Unknown
        );
        assert_eq!(
            Attachment::Attached.step(AttachmentEvent::Uncertain),
            Attachment::UncertainAttached
        );
        assert_eq!(
            Attachment::UncertainAttached.step(AttachmentEvent::Disconnected),
            Attachment::Detached
        );
        for value in 5..=255 {
            assert_eq!(
                StorageShareStatus::from_byte(value),
                StorageShareStatus::Unknown
            );
        }
    }
}

#[cfg(test)]
mod transition_matrix {
    use super::*;
    #[test]
    fn every_mount_phase_rejects_late_or_wrong_port_confirmation() {
        for state in [
            State::Waiting,
            State::Confirmed,
            State::Rejected,
            State::Unknown,
        ] {
            for port in [None, Some(123), Some(124)] {
                for bits in 0..8 {
                    for now in [Duration::ZERO, Duration::from_secs(10)] {
                        let event = Observation {
                            port,
                            connected: bits & 1 != 0,
                            failed: bits & 2 != 0,
                            peer_ready: bits & 4 != 0,
                        };
                        let model = Mount {
                            state,
                            port: 123,
                            deadline: Duration::from_secs(10),
                        };
                        let next = model.step(event, now);
                        assert_eq!(next, model.step(event, now));
                        if state != State::Waiting {
                            assert_eq!(next, model);
                        } else if now >= model.deadline {
                            assert_eq!(next.state(), State::Unknown);
                        } else if next.state() == State::Confirmed {
                            assert_eq!(port, Some(123));
                            assert!(event.connected && event.peer_ready && !event.failed);
                        }
                    }
                }
            }
        }
    }
}

//! Session authority. A transport epoch invalidates observations from old sockets.
use crate::Transition;

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState { Connecting, Online, Recovering, Closed, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Established { epoch: u64 },
    TransportLost { epoch: u64, recoverable: bool },
    ProtocolFault { epoch: u64 },
    RecoveryFailed { epoch: u64 },
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEffect {
    Online,
    Recover { epoch: u64 },
    Fail,
    Close,
    IgnoreStale,
    IgnoreTerminal,
    IgnoreDuplicate,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Session {
    state: SessionState,
    epoch: u64,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            state: SessionState::Connecting,
            epoch: 0,
        }
    }
}
impl Session {
    pub fn state(&self) -> SessionState {
        self.state
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn step(mut self, event: SessionEvent) -> Transition<Self, SessionEffect> {
        let effect = match self.state {
            SessionState::Closed | SessionState::Failed => SessionEffect::IgnoreTerminal,
            SessionState::Connecting | SessionState::Online | SessionState::Recovering => {
                match event {
                    SessionEvent::Stop => {
                        self.state = SessionState::Closed;
                        SessionEffect::Close
                    }
                    SessionEvent::Established { epoch } => {
                        if epoch != self.epoch {
                            SessionEffect::IgnoreStale
                        } else if self.state == SessionState::Online {
                            SessionEffect::IgnoreDuplicate
                        } else {
                            self.state = SessionState::Online;
                            SessionEffect::Online
                        }
                    }
                    SessionEvent::TransportLost { epoch, recoverable } => {
                        if epoch != self.epoch {
                            SessionEffect::IgnoreStale
                        } else if self.state == SessionState::Recovering {
                            SessionEffect::IgnoreDuplicate
                        } else if recoverable && self.state == SessionState::Online {
                            if let Some(epoch) = self.epoch.checked_add(1) {
                                self.epoch = epoch;
                                self.state = SessionState::Recovering;
                                SessionEffect::Recover { epoch }
                            } else {
                                self.state = SessionState::Failed;
                                SessionEffect::Fail
                            }
                        } else {
                            self.state = SessionState::Failed;
                            SessionEffect::Fail
                        }
                    }
                    SessionEvent::ProtocolFault { epoch }
                    | SessionEvent::RecoveryFailed { epoch } => {
                        if epoch != self.epoch {
                            SessionEffect::IgnoreStale
                        } else {
                            self.state = SessionState::Failed;
                            SessionEffect::Fail
                        }
                    }
                }
            }
        };
        Transition {
            state: self,
            effect,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_session_phases_and_events_are_total_and_deterministic() {
        for &state in SessionState::ALL {
            for epoch in [0, 1, u64::MAX] {
                for event in [
                    SessionEvent::Established { epoch },
                    SessionEvent::TransportLost {
                        epoch,
                        recoverable: true,
                    },
                    SessionEvent::TransportLost {
                        epoch,
                        recoverable: false,
                    },
                    SessionEvent::ProtocolFault { epoch },
                    SessionEvent::RecoveryFailed { epoch },
                    SessionEvent::Stop,
                ] {
                    let model = Session { state, epoch: 1 };
                    let transition = model.step(event);
                    assert_eq!(transition, model.step(event));
                    if matches!(state, SessionState::Closed | SessionState::Failed) {
                        assert_eq!(transition.state, model);
                    }
                }
            }
        }
    }
    #[test]
    fn old_socket_cannot_confirm_recovery_and_exhausted_epochs_never_wrap() {
        let online = Session::default()
            .step(SessionEvent::Established { epoch: 0 })
            .state;
        let recovery = online.step(SessionEvent::TransportLost {
            epoch: 0,
            recoverable: true,
        });
        assert_eq!(recovery.effect, SessionEffect::Recover { epoch: 1 });
        let stale = recovery.state.step(SessionEvent::Established { epoch: 0 });
        assert_eq!(stale.effect, SessionEffect::IgnoreStale);
        assert_eq!(stale.state.state(), SessionState::Recovering);
        assert_eq!(
            stale
                .state
                .step(SessionEvent::Stop)
                .state
                .step(SessionEvent::Established { epoch: 1 })
                .state
                .state(),
            SessionState::Closed
        );
        assert_eq!(
            Session {
                state: SessionState::Online,
                epoch: u64::MAX
            }
            .step(SessionEvent::TransportLost {
                epoch: u64::MAX,
                recoverable: true
            })
            .effect,
            SessionEffect::Fail
        );
    }
}

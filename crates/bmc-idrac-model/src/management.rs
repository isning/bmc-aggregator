//! HTTP connection ownership: incomplete exchanges cannot return to the cache.
use crate::Transition;
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeState { Checking, Connecting, Sending, Receiving, Complete, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeEvent {
    ConnectionReady { ready: bool },
    Connected,
    Headers { close: bool },
    BodyComplete,
    Fault,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeEffect {
    Open,
    Send,
    ReadBody,
    Reuse,
    Discard,
    Reject,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exchange {
    state: ExchangeState,
    close: bool,
}
impl Default for Exchange {
    fn default() -> Self {
        Self {
            state: ExchangeState::Checking,
            close: false,
        }
    }
}
impl Exchange {
    pub fn state(&self) -> ExchangeState {
        self.state
    }
    pub fn step(mut self, event: ExchangeEvent) -> Transition<Self, ExchangeEffect> {
        let effect = match self.state {
            ExchangeState::Complete | ExchangeState::Failed => ExchangeEffect::IgnoreTerminal,
            ExchangeState::Checking
            | ExchangeState::Connecting
            | ExchangeState::Sending
            | ExchangeState::Receiving => match event {
                ExchangeEvent::Fault | ExchangeEvent::Stop => {
                    self.state = ExchangeState::Failed;
                    ExchangeEffect::Discard
                }
                ExchangeEvent::ConnectionReady { ready } => {
                    if self.state != ExchangeState::Checking {
                        self.state = ExchangeState::Failed;
                        ExchangeEffect::Reject
                    } else if ready {
                        self.state = ExchangeState::Sending;
                        ExchangeEffect::Send
                    } else {
                        self.state = ExchangeState::Connecting;
                        ExchangeEffect::Open
                    }
                }
                ExchangeEvent::Connected => {
                    if self.state == ExchangeState::Connecting {
                        self.state = ExchangeState::Sending;
                        ExchangeEffect::Send
                    } else {
                        self.state = ExchangeState::Failed;
                        ExchangeEffect::Reject
                    }
                }
                ExchangeEvent::Headers { close } => {
                    if self.state == ExchangeState::Sending {
                        self.state = ExchangeState::Receiving;
                        self.close = close;
                        ExchangeEffect::ReadBody
                    } else {
                        self.state = ExchangeState::Failed;
                        ExchangeEffect::Reject
                    }
                }
                ExchangeEvent::BodyComplete => {
                    if self.state == ExchangeState::Receiving {
                        self.state = ExchangeState::Complete;
                        if self.close {
                            ExchangeEffect::Discard
                        } else {
                            ExchangeEffect::Reuse
                        }
                    } else {
                        self.state = ExchangeState::Failed;
                        ExchangeEffect::Reject
                    }
                }
            },
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
    fn only_complete_body_may_return_a_connection_to_cache() {
        for &state in ExchangeState::ALL {
            for event in [
                ExchangeEvent::ConnectionReady { ready: true },
                ExchangeEvent::ConnectionReady { ready: false },
                ExchangeEvent::Connected,
                ExchangeEvent::Headers { close: true },
                ExchangeEvent::Headers { close: false },
                ExchangeEvent::BodyComplete,
                ExchangeEvent::Fault,
                ExchangeEvent::Stop,
            ] {
                let model = Exchange {
                    state,
                    close: false,
                };
                let transition = model.step(event);
                assert_eq!(
                    transition.effect == ExchangeEffect::Reuse,
                    state == ExchangeState::Receiving && event == ExchangeEvent::BodyComplete
                );
                if matches!(state, ExchangeState::Complete | ExchangeState::Failed) {
                    assert_eq!(transition.state, model);
                }
            }
        }
        let started = Exchange::default()
            .step(ExchangeEvent::ConnectionReady { ready: false })
            .state
            .step(ExchangeEvent::Connected)
            .state;
        let headers = started.step(ExchangeEvent::Headers { close: true });
        assert_eq!(
            headers.state.step(ExchangeEvent::BodyComplete).effect,
            ExchangeEffect::Discard
        );
        assert_eq!(
            headers
                .state
                .step(ExchangeEvent::Fault)
                .state
                .step(ExchangeEvent::BodyComplete)
                .effect,
            ExchangeEffect::IgnoreTerminal
        );
    }
}

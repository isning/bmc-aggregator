//! Knowledge about one remote mutation. A write is never a confirmation.
use crate::Transition;
use core::time::Duration;

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState { AwaitingWrite, AwaitingObservation, Confirmed, Rejected, Unknown, Cancelled }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationEvent {
    WriteFinished { now: Duration, accepted: bool },
    Observation { now: Duration, matches: bool },
    Tick { now: Duration },
    RemoteRejected,
    TransportLost,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationEffect {
    Wait,
    Observe,
    Confirm,
    Reject,
    ReportUnknown,
    Cancel,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operation {
    state: OperationState,
    deadline: Duration,
}
impl Operation {
    pub fn new(now: Duration, budget: Duration) -> Self {
        Self {
            state: OperationState::AwaitingWrite,
            deadline: now.saturating_add(budget),
        }
    }
    pub fn state(&self) -> OperationState {
        self.state
    }
    pub fn deadline(&self) -> Duration {
        self.deadline
    }
    pub fn step(mut self, event: OperationEvent) -> Transition<Self, OperationEffect> {
        let effect = match self.state {
            OperationState::Confirmed
            | OperationState::Rejected
            | OperationState::Unknown
            | OperationState::Cancelled => OperationEffect::IgnoreTerminal,
            OperationState::AwaitingWrite | OperationState::AwaitingObservation => match event {
                OperationEvent::Stop => {
                    self.state = OperationState::Cancelled;
                    OperationEffect::Cancel
                }
                OperationEvent::RemoteRejected => {
                    self.state = OperationState::Rejected;
                    OperationEffect::Reject
                }
                OperationEvent::TransportLost => {
                    self.state = OperationState::Unknown;
                    OperationEffect::ReportUnknown
                }
                OperationEvent::Tick { now } => {
                    if now >= self.deadline {
                        self.state = OperationState::Unknown;
                        OperationEffect::ReportUnknown
                    } else {
                        OperationEffect::Wait
                    }
                }
                OperationEvent::WriteFinished { now, accepted } => {
                    if now >= self.deadline {
                        self.state = OperationState::Unknown;
                        OperationEffect::ReportUnknown
                    } else if self.state == OperationState::AwaitingObservation {
                        OperationEffect::Wait
                    } else if accepted {
                        self.state = OperationState::AwaitingObservation;
                        OperationEffect::Observe
                    } else {
                        self.state = OperationState::Rejected;
                        OperationEffect::Reject
                    }
                }
                OperationEvent::Observation { now, matches } => {
                    if now >= self.deadline {
                        self.state = OperationState::Unknown;
                        OperationEffect::ReportUnknown
                    } else if self.state == OperationState::AwaitingWrite {
                        OperationEffect::Wait
                    } else if matches {
                        self.state = OperationState::Confirmed;
                        OperationEffect::Confirm
                    } else {
                        OperationEffect::Observe
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
    fn matrix_and_terminal_states_never_revive() {
        let events = [
            OperationEvent::WriteFinished {
                now: Duration::ZERO,
                accepted: true,
            },
            OperationEvent::WriteFinished {
                now: Duration::ZERO,
                accepted: false,
            },
            OperationEvent::Observation {
                now: Duration::ZERO,
                matches: true,
            },
            OperationEvent::Observation {
                now: Duration::ZERO,
                matches: false,
            },
            OperationEvent::Tick {
                now: Duration::from_secs(10),
            },
            OperationEvent::RemoteRejected,
            OperationEvent::TransportLost,
            OperationEvent::Stop,
        ];
        for &state in OperationState::ALL {
            for event in events {
                let model = Operation {
                    state,
                    deadline: Duration::from_secs(10),
                };
                let transition = model.step(event);
                assert_eq!(transition, model.step(event));
                if matches!(
                    state,
                    OperationState::Confirmed
                        | OperationState::Rejected
                        | OperationState::Unknown
                        | OperationState::Cancelled
                ) {
                    assert_eq!(transition.state.state(), state);
                    assert_eq!(transition.effect, OperationEffect::IgnoreTerminal);
                }
            }
        }
    }
    #[test]
    fn early_observation_and_local_write_do_not_confirm_and_late_success_is_unknown() {
        let model = Operation::new(Duration::ZERO, Duration::from_secs(10));
        let early = model.step(OperationEvent::Observation {
            now: Duration::ZERO,
            matches: true,
        });
        assert_eq!(early.state.state(), OperationState::AwaitingWrite);
        let written = early.state.step(OperationEvent::WriteFinished {
            now: Duration::ZERO,
            accepted: true,
        });
        assert_eq!(written.state.state(), OperationState::AwaitingObservation);
        let late = written.state.step(OperationEvent::Observation {
            now: Duration::from_secs(10),
            matches: true,
        });
        assert_eq!(late.state.state(), OperationState::Unknown);
    }
    #[test]
    fn mismatching_progress_does_not_extend_absolute_deadline() {
        let mut model = Operation::new(Duration::ZERO, Duration::from_secs(10))
            .step(OperationEvent::WriteFinished {
                now: Duration::ZERO,
                accepted: true,
            })
            .state;
        for second in 0..10 {
            model = model
                .step(OperationEvent::Observation {
                    now: Duration::from_secs(second),
                    matches: false,
                })
                .state;
            assert_eq!(model.deadline(), Duration::from_secs(10));
        }
        assert_eq!(
            model
                .step(OperationEvent::Tick {
                    now: Duration::from_secs(10)
                })
                .effect,
            OperationEffect::ReportUnknown
        );
    }
}

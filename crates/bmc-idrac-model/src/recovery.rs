//! Bounded AVCT recovery, including cancellation and unexpected completions.
use crate::Transition;
use core::time::Duration;

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryState { Ready, Connecting, Backoff, Complete, Failed, Cancelled }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryEvent {
    Tick { now: Duration },
    Finished { now: Duration, success: bool },
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryEffect {
    Connect { remaining: Duration },
    Sleep(Duration),
    Complete,
    Fail,
    Cancel,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovery {
    state: RecoveryState,
    attempts: u8,
    deadline: Duration,
    retry_at: Duration,
}
impl Recovery {
    pub fn new(now: Duration) -> Self {
        // Three 30-second setup attempts and two five-second delays.
        Self {
            state: RecoveryState::Ready,
            attempts: 0,
            deadline: now.saturating_add(Duration::from_secs(100)),
            retry_at: now,
        }
    }
    pub fn state(&self) -> RecoveryState {
        self.state
    }
    pub fn step(mut self, event: RecoveryEvent) -> Transition<Self, RecoveryEffect> {
        let effect = match self.state {
            RecoveryState::Complete | RecoveryState::Failed | RecoveryState::Cancelled => {
                RecoveryEffect::IgnoreTerminal
            }
            RecoveryState::Ready | RecoveryState::Connecting | RecoveryState::Backoff => {
                match event {
                    RecoveryEvent::Stop => {
                        self.state = RecoveryState::Cancelled;
                        RecoveryEffect::Cancel
                    }
                    RecoveryEvent::Tick { now } => {
                        if now >= self.deadline {
                            self.state = RecoveryState::Failed;
                            RecoveryEffect::Fail
                        } else if self.state == RecoveryState::Connecting {
                            RecoveryEffect::Sleep(self.deadline - now)
                        } else if now < self.retry_at {
                            RecoveryEffect::Sleep(self.retry_at - now)
                        } else {
                            self.attempts += 1;
                            self.state = RecoveryState::Connecting;
                            RecoveryEffect::Connect {
                                remaining: Duration::from_secs(30).min(self.deadline - now),
                            }
                        }
                    }
                    RecoveryEvent::Finished { now, success } => {
                        if self.state != RecoveryState::Connecting || now >= self.deadline {
                            self.state = RecoveryState::Failed;
                            RecoveryEffect::Fail
                        } else if success {
                            self.state = RecoveryState::Complete;
                            RecoveryEffect::Complete
                        } else if self.attempts >= 3 {
                            self.state = RecoveryState::Failed;
                            RecoveryEffect::Fail
                        } else {
                            self.state = RecoveryState::Backoff;
                            self.retry_at = now.saturating_add(Duration::from_secs(5));
                            RecoveryEffect::Sleep(Duration::from_secs(5).min(self.deadline - now))
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
    fn attempts_are_bounded_and_duplicate_ticks_do_not_create_parallel_connections() {
        let mut model = Recovery::new(Duration::ZERO);
        for attempt in 0..3 {
            let now = Duration::from_secs(attempt * 5);
            let start = model.step(RecoveryEvent::Tick { now });
            assert!(matches!(start.effect, RecoveryEffect::Connect { .. }));
            assert!(matches!(
                start.state.step(RecoveryEvent::Tick { now }).effect,
                RecoveryEffect::Sleep(_)
            ));
            let finish = start.state.step(RecoveryEvent::Finished {
                now,
                success: false,
            });
            model = finish.state;
        }
        assert_eq!(model.state(), RecoveryState::Failed);
    }
    #[test]
    fn every_phase_handles_every_event_and_terminal_completion_cannot_revive_it() {
        for &state in RecoveryState::ALL {
            for event in [
                RecoveryEvent::Tick {
                    now: Duration::ZERO,
                },
                RecoveryEvent::Tick {
                    now: Duration::from_secs(100),
                },
                RecoveryEvent::Finished {
                    now: Duration::ZERO,
                    success: true,
                },
                RecoveryEvent::Finished {
                    now: Duration::ZERO,
                    success: false,
                },
                RecoveryEvent::Stop,
            ] {
                let model = Recovery {
                    state,
                    attempts: 1,
                    deadline: Duration::from_secs(100),
                    retry_at: Duration::ZERO,
                };
                let transition = model.step(event);
                assert_eq!(transition, model.step(event));
                if matches!(
                    state,
                    RecoveryState::Complete | RecoveryState::Failed | RecoveryState::Cancelled
                ) {
                    assert_eq!(transition.state, model);
                }
            }
        }
        let late = Recovery::new(Duration::ZERO)
            .step(RecoveryEvent::Tick {
                now: Duration::ZERO,
            })
            .state
            .step(RecoveryEvent::Finished {
                now: Duration::from_secs(100),
                success: true,
            });
        assert_eq!(late.state.state(), RecoveryState::Failed);
    }
}

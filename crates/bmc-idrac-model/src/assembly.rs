//! Bounded assembly of a display command across complete wire records.
use crate::Transition;
use core::time::Duration;
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssemblyPhase { Idle, Decoding, Collecting, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssemblyEvent {
    Input {
        now: Duration,
        retained: usize,
        incoming: usize,
    },
    Decoded {
        pending: bool,
    },
    Tick {
        now: Duration,
    },
    Fault,
    Reset,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssemblyEffect {
    Decode,
    Wait,
    Reject,
    Reset,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Assembly {
    phase: AssemblyPhase,
    deadline: Option<Duration>,
    fragments: u16,
}
impl Default for Assembly {
    fn default() -> Self {
        Self {
            phase: AssemblyPhase::Idle,
            deadline: None,
            fragments: 0,
        }
    }
}
impl Assembly {
    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }
    pub fn step(mut self, event: AssemblyEvent) -> Transition<Self, AssemblyEffect> {
        let effect = match event {
            AssemblyEvent::Reset => {
                self = Self::default();
                AssemblyEffect::Reset
            }
            AssemblyEvent::Fault => {
                self.phase = AssemblyPhase::Failed;
                AssemblyEffect::Reject
            }
            AssemblyEvent::Tick { now } => {
                if self.phase == AssemblyPhase::Failed
                    || self.deadline.is_some_and(|deadline| now >= deadline)
                {
                    self.phase = AssemblyPhase::Failed;
                    AssemblyEffect::Reject
                } else {
                    AssemblyEffect::Wait
                }
            }
            AssemblyEvent::Input {
                now,
                retained,
                incoming,
            } => {
                let valid = matches!(self.phase, AssemblyPhase::Idle | AssemblyPhase::Collecting)
                    && self.deadline.is_none_or(|deadline| now < deadline)
                    && self.fragments < 4096
                    && retained
                        .checked_add(incoming)
                        .is_some_and(|bytes| bytes <= 64 * 1024 * 1024);
                if valid {
                    if self.deadline.is_none() {
                        self.deadline = Some(now.saturating_add(Duration::from_secs(10)));
                    }
                    self.fragments += 1;
                    self.phase = AssemblyPhase::Decoding;
                    AssemblyEffect::Decode
                } else {
                    self.phase = AssemblyPhase::Failed;
                    AssemblyEffect::Reject
                }
            }
            AssemblyEvent::Decoded { pending } => {
                if self.phase != AssemblyPhase::Decoding {
                    self.phase = AssemblyPhase::Failed;
                    AssemblyEffect::Reject
                } else if pending {
                    self.phase = AssemblyPhase::Collecting;
                    AssemblyEffect::Wait
                } else {
                    self = Self::default();
                    AssemblyEffect::Wait
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
    fn every_phase_and_event_is_total_and_failed_work_requires_explicit_reset() {
        for &phase in AssemblyPhase::ALL {
            for event in [
                AssemblyEvent::Input {
                    now: Duration::ZERO,
                    retained: 0,
                    incoming: 1,
                },
                AssemblyEvent::Input {
                    now: Duration::ZERO,
                    retained: usize::MAX,
                    incoming: 1,
                },
                AssemblyEvent::Decoded { pending: true },
                AssemblyEvent::Decoded { pending: false },
                AssemblyEvent::Tick {
                    now: Duration::from_secs(10),
                },
                AssemblyEvent::Fault,
                AssemblyEvent::Reset,
            ] {
                let model = Assembly {
                    phase,
                    deadline: Some(Duration::from_secs(10)),
                    fragments: 1,
                };
                let transition = model.step(event);
                assert_eq!(transition, model.step(event));
                if phase == AssemblyPhase::Failed && event != AssemblyEvent::Reset {
                    assert_eq!(transition.state.phase, AssemblyPhase::Failed);
                }
            }
        }
    }
    #[test]
    fn partial_progress_cannot_extend_time_memory_or_fragment_budget() {
        let first = Assembly::default()
            .step(AssemblyEvent::Input {
                now: Duration::ZERO,
                retained: 0,
                incoming: 1,
            })
            .state
            .step(AssemblyEvent::Decoded { pending: true })
            .state;
        assert_eq!(
            first
                .step(AssemblyEvent::Input {
                    now: Duration::from_secs(10),
                    retained: 1,
                    incoming: 1
                })
                .effect,
            AssemblyEffect::Reject
        );
        assert_eq!(
            first
                .step(AssemblyEvent::Input {
                    now: Duration::ZERO,
                    retained: 64 * 1024 * 1024,
                    incoming: 1
                })
                .effect,
            AssemblyEffect::Reject
        );
        let exhausted = Assembly {
            fragments: 4096,
            ..first
        };
        assert_eq!(
            exhausted
                .step(AssemblyEvent::Input {
                    now: Duration::ZERO,
                    retained: 1,
                    incoming: 1
                })
                .effect,
            AssemblyEffect::Reject
        );
    }
}

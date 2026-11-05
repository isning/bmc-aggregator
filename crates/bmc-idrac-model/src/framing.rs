//! Incremental AVMP framing. Progress belongs to the session, not a read future.
use crate::Transition;
use alloc::vec::Vec;
use core::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramingFault {
    InvalidMagic,
    InvalidLength,
    ExcessData,
    Truncated,
    TimedOut,
    Cancelled,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramingState {
    Header,
    Payload,
    Failed(FramingFault),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramingEvent<'a> {
    Bytes { bytes: &'a [u8], now: Duration },
    Tick { now: Duration },
    Eof,
    Stop,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramingEffect {
    More,
    Packet(Vec<u8>),
    Reject(FramingFault),
    IgnoreTerminal,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvmpFraming {
    state: FramingState,
    bytes: Vec<u8>,
    target: usize,
    deadline: Option<Duration>,
}
impl Default for AvmpFraming {
    fn default() -> Self {
        Self {
            state: FramingState::Header,
            bytes: Vec::new(),
            target: 12,
            deadline: None,
        }
    }
}
impl AvmpFraming {
    pub fn state(&self) -> &FramingState {
        &self.state
    }
    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }
    pub fn needed(&self) -> usize {
        self.target.saturating_sub(self.bytes.len())
    }
    pub fn step(mut self, event: FramingEvent<'_>) -> Transition<Self, FramingEffect> {
        let effect = match self.state {
            FramingState::Failed(_) => FramingEffect::IgnoreTerminal,
            FramingState::Header | FramingState::Payload => {
                let fault = match event {
                    FramingEvent::Stop => Some(FramingFault::Cancelled),
                    FramingEvent::Eof => Some(FramingFault::Truncated),
                    FramingEvent::Tick { now } => {
                        if self.deadline.is_some_and(|deadline| now >= deadline) {
                            Some(FramingFault::TimedOut)
                        } else {
                            None
                        }
                    }
                    FramingEvent::Bytes { bytes, now } => {
                        if self.deadline.is_some_and(|deadline| now >= deadline) {
                            Some(FramingFault::TimedOut)
                        } else if bytes.len() > self.needed() {
                            Some(FramingFault::ExcessData)
                        } else {
                            if !bytes.is_empty() && self.deadline.is_none() {
                                self.deadline = Some(now.saturating_add(Duration::from_secs(10)));
                            }
                            self.bytes.extend_from_slice(bytes);
                            if self.state == FramingState::Header && self.bytes.len() == 12 {
                                let magic = &self.bytes[..4];
                                let total = u32::from_be_bytes([
                                    self.bytes[4],
                                    self.bytes[5],
                                    self.bytes[6],
                                    self.bytes[7],
                                ]) as usize;
                                if magic != b"AVMP" && magic != b"APCP" {
                                    Some(FramingFault::InvalidMagic)
                                } else if !(12..=1024 * 1024).contains(&total) {
                                    Some(FramingFault::InvalidLength)
                                } else {
                                    self.target = total;
                                    self.state = FramingState::Payload;
                                    None
                                }
                            } else {
                                None
                            }
                        }
                    }
                };
                if let Some(fault) = fault {
                    self.state = FramingState::Failed(fault);
                    FramingEffect::Reject(fault)
                } else if self.state == FramingState::Payload && self.bytes.len() == self.target {
                    let packet = core::mem::take(&mut self.bytes);
                    self = Self::default();
                    FramingEffect::Packet(packet)
                } else {
                    FramingEffect::More
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
    fn all_fragment_boundaries_preserve_frame_and_reset_only_on_complete_record() {
        let bytes = b"AVMP\0\0\0\x10\x82\0\0\0abcd";
        for chunk_size in 1..=12 {
            let mut model = AvmpFraming::default();
            let mut offset = 0;
            let mut packet = None;
            while offset < bytes.len() {
                let count = chunk_size.min(model.needed()).min(bytes.len() - offset);
                let transition = model.step(FramingEvent::Bytes {
                    bytes: &bytes[offset..offset + count],
                    now: Duration::ZERO,
                });
                model = transition.state;
                if let FramingEffect::Packet(bytes) = transition.effect {
                    packet = Some(bytes);
                }
                offset += count;
            }
            assert_eq!(packet.as_deref(), Some(bytes.as_slice()));
            assert_eq!(model, AvmpFraming::default());
        }
    }
    #[test]
    fn malformed_lengths_magic_eof_and_terminal_inputs_are_explicit() {
        for length in [0u32, 11, 1024 * 1024 + 1, u32::MAX] {
            let mut header = *b"AVMP\0\0\0\0\0\0\0\0";
            header[4..8].copy_from_slice(&length.to_be_bytes());
            let bad = AvmpFraming::default().step(FramingEvent::Bytes {
                bytes: &header,
                now: Duration::ZERO,
            });
            assert_eq!(
                bad.effect,
                FramingEffect::Reject(FramingFault::InvalidLength)
            );
            assert_eq!(
                bad.state
                    .step(FramingEvent::Bytes {
                        bytes: b"AVMP",
                        now: Duration::ZERO
                    })
                    .effect,
                FramingEffect::IgnoreTerminal
            );
        }
        assert_eq!(
            AvmpFraming::default()
                .step(FramingEvent::Bytes {
                    bytes: b"JUNK\0\0\0\x0c\0\0\0\0",
                    now: Duration::ZERO
                })
                .effect,
            FramingEffect::Reject(FramingFault::InvalidMagic)
        );
        for state in [
            AvmpFraming::default(),
            AvmpFraming::default()
                .step(FramingEvent::Bytes {
                    bytes: b"AV",
                    now: Duration::ZERO,
                })
                .state,
        ] {
            assert_eq!(
                state.step(FramingEvent::Eof).effect,
                FramingEffect::Reject(FramingFault::Truncated)
            );
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn fragmented_progress_cannot_extend_deadline_but_idle_has_no_partial_record_deadline() {
        let idle = AvmpFraming::default().step(FramingEvent::Tick {
            now: Duration::from_secs(9999),
        });
        assert_eq!(idle.effect, FramingEffect::More);
        let first = idle.state.step(FramingEvent::Bytes {
            bytes: b"A",
            now: Duration::ZERO,
        });
        let second = first.state.step(FramingEvent::Bytes {
            bytes: b"V",
            now: Duration::from_secs(9),
        });
        assert_eq!(second.state.deadline(), Some(Duration::from_secs(10)));
        assert_eq!(
            second
                .state
                .step(FramingEvent::Tick {
                    now: Duration::from_secs(10)
                })
                .effect,
            FramingEffect::Reject(FramingFault::TimedOut)
        );
    }
}

//! Readonly-source ServerImage transfer sequencing. Never replay a mutation.
use crate::Transition;
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadState { Inspecting, Creating, Transferring, Finalizing, Verifying, Complete, Failed, Unknown }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadEvent {
    Catalog { empty: bool, writable: bool },
    Created { accepted: bool },
    DataAcknowledged { accepted: bool },
    Finalized { accepted: bool },
    Verified { matches: bool },
    Fault,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadEffect {
    Create { blocks: u32 },
    Chunk { offset_blocks: u32, bytes: usize },
    Finalize,
    QueryCatalog,
    Complete,
    Reject,
    ReportUnknown,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Upload {
    state: UploadState,
    size: u64,
    transferred: u64,
    pending: u64,
}
impl Upload {
    pub fn new(size: u64) -> Self {
        Self {
            state: UploadState::Inspecting,
            size,
            transferred: 0,
            pending: 0,
        }
    }
    pub fn state(&self) -> UploadState {
        self.state
    }
    fn next_chunk(&mut self) -> UploadEffect {
        if self.transferred == self.size {
            self.state = UploadState::Finalizing;
            UploadEffect::Finalize
        } else {
            self.state = UploadState::Transferring;
            self.pending = (self.size - self.transferred).min(32768);
            UploadEffect::Chunk {
                offset_blocks: (self.transferred / 512) as u32,
                bytes: self.pending as usize,
            }
        }
    }
    pub fn step(mut self, event: UploadEvent) -> Transition<Self, UploadEffect> {
        let effect = match self.state {
            UploadState::Complete | UploadState::Failed | UploadState::Unknown => {
                UploadEffect::IgnoreTerminal
            }
            UploadState::Inspecting
            | UploadState::Creating
            | UploadState::Transferring
            | UploadState::Finalizing
            | UploadState::Verifying => match event {
                UploadEvent::Stop | UploadEvent::Fault => {
                    self.state = UploadState::Unknown;
                    UploadEffect::ReportUnknown
                }
                UploadEvent::Catalog { empty, writable } => {
                    if self.state == UploadState::Inspecting
                        && empty
                        && writable
                        && self.size > 0
                        && self.size.div_ceil(512) <= u64::from(u32::MAX)
                    {
                        self.state = UploadState::Creating;
                        UploadEffect::Create {
                            blocks: self.size.div_ceil(512) as u32,
                        }
                    } else {
                        self.state = UploadState::Failed;
                        UploadEffect::Reject
                    }
                }
                UploadEvent::Created { accepted } => {
                    if self.state == UploadState::Creating && accepted {
                        self.next_chunk()
                    } else {
                        self.state = UploadState::Failed;
                        UploadEffect::Reject
                    }
                }
                UploadEvent::DataAcknowledged { accepted } => {
                    if self.state == UploadState::Transferring && accepted {
                        self.transferred += self.pending;
                        self.pending = 0;
                        self.next_chunk()
                    } else {
                        self.state = UploadState::Failed;
                        UploadEffect::Reject
                    }
                }
                UploadEvent::Finalized { accepted } => {
                    if self.state == UploadState::Finalizing && accepted {
                        self.state = UploadState::Verifying;
                        UploadEffect::QueryCatalog
                    } else {
                        self.state = UploadState::Failed;
                        UploadEffect::Reject
                    }
                }
                UploadEvent::Verified { matches } => {
                    if self.state == UploadState::Verifying && matches {
                        self.state = UploadState::Complete;
                        UploadEffect::Complete
                    } else {
                        self.state = UploadState::Unknown;
                        UploadEffect::ReportUnknown
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
    fn every_phase_event_and_size_limit_has_an_explicit_result() {
        for &state in UploadState::ALL {
            for size in [0, 1, 32768, u64::MAX] {
                for event in [
                    UploadEvent::Catalog {
                        empty: true,
                        writable: true,
                    },
                    UploadEvent::Catalog {
                        empty: false,
                        writable: false,
                    },
                    UploadEvent::Created { accepted: true },
                    UploadEvent::Created { accepted: false },
                    UploadEvent::DataAcknowledged { accepted: true },
                    UploadEvent::DataAcknowledged { accepted: false },
                    UploadEvent::Finalized { accepted: true },
                    UploadEvent::Finalized { accepted: false },
                    UploadEvent::Verified { matches: true },
                    UploadEvent::Verified { matches: false },
                    UploadEvent::Fault,
                    UploadEvent::Stop,
                ] {
                    let model = Upload {
                        state,
                        size,
                        transferred: 0,
                        pending: 0,
                    };
                    let transition = model.step(event);
                    assert_eq!(transition, model.step(event));
                    if matches!(
                        state,
                        UploadState::Complete | UploadState::Failed | UploadState::Unknown
                    ) {
                        assert_eq!(transition.state, model);
                    }
                }
            }
        }
    }
    #[test]
    fn data_offsets_follow_acknowledged_bytes_and_finalize_needs_inventory_readback() {
        let created = Upload::new(32769)
            .step(UploadEvent::Catalog {
                empty: true,
                writable: true,
            })
            .state
            .step(UploadEvent::Created { accepted: true });
        assert_eq!(
            created.effect,
            UploadEffect::Chunk {
                offset_blocks: 0,
                bytes: 32768
            }
        );
        let last = created
            .state
            .step(UploadEvent::DataAcknowledged { accepted: true });
        assert_eq!(
            last.effect,
            UploadEffect::Chunk {
                offset_blocks: 64,
                bytes: 1
            }
        );
        let finalize = last
            .state
            .step(UploadEvent::DataAcknowledged { accepted: true });
        assert_eq!(finalize.effect, UploadEffect::Finalize);
        let verify = finalize
            .state
            .step(UploadEvent::Finalized { accepted: true });
        assert_eq!(verify.effect, UploadEffect::QueryCatalog);
        assert_eq!(
            verify
                .state
                .step(UploadEvent::Verified { matches: false })
                .state
                .state(),
            UploadState::Unknown
        );
        assert_eq!(
            verify
                .state
                .step(UploadEvent::Verified { matches: true })
                .state
                .state(),
            UploadState::Complete
        );
    }
}

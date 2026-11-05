//! AVMP reconnect scheduling and drive-layout acknowledgement decisions.
use crate::Transition;
use core::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectState {
    WaitingForSilence,
    Attempting { deadline: Duration },
    Complete,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectEvent {
    Tick { now: Duration },
    AttemptFinished { now: Duration, success: bool },
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectEffect {
    Sleep(Duration),
    Connect { remaining: Duration },
    Complete,
    Fail,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reconnect {
    state: ReconnectState,
    last_packet: Duration,
    check_timeout: Duration,
}
impl Reconnect {
    pub fn new(last_packet: Duration, check_timeout: Duration) -> Self {
        Self {
            state: ReconnectState::WaitingForSilence,
            last_packet,
            check_timeout,
        }
    }
    pub fn state(&self) -> ReconnectState {
        self.state
    }
    pub fn due_at(&self, now: Duration) -> bool {
        now.saturating_sub(self.last_packet) > self.check_timeout
    }
    pub fn step(mut self, event: ReconnectEvent) -> Transition<Self, ReconnectEffect> {
        let effect = match self.state {
            ReconnectState::Complete | ReconnectState::Failed => ReconnectEffect::IgnoreTerminal,
            ReconnectState::WaitingForSilence => match event {
                ReconnectEvent::Stop | ReconnectEvent::AttemptFinished { .. } => {
                    self.state = ReconnectState::Failed;
                    ReconnectEffect::Fail
                }
                ReconnectEvent::Tick { now } => {
                    if self.due_at(now) {
                        let remaining = Duration::from_secs(180);
                        self.state = ReconnectState::Attempting {
                            deadline: now.saturating_add(remaining),
                        };
                        ReconnectEffect::Connect { remaining }
                    } else {
                        ReconnectEffect::Sleep(
                            (self.check_timeout / 2).max(Duration::from_nanos(1)),
                        )
                    }
                }
            },
            ReconnectState::Attempting { deadline } => match event {
                ReconnectEvent::Stop => {
                    self.state = ReconnectState::Failed;
                    ReconnectEffect::Fail
                }
                ReconnectEvent::Tick { now } => {
                    if now >= deadline {
                        self.state = ReconnectState::Failed;
                        ReconnectEffect::Fail
                    } else {
                        ReconnectEffect::Connect {
                            remaining: deadline - now,
                        }
                    }
                }
                ReconnectEvent::AttemptFinished { now, success } => {
                    if now >= deadline {
                        self.state = ReconnectState::Failed;
                        ReconnectEffect::Fail
                    } else if success {
                        self.state = ReconnectState::Complete;
                        ReconnectEffect::Complete
                    } else {
                        ReconnectEffect::Sleep(Duration::from_secs(5).min(deadline - now))
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

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriveRefreshState { Discovering, AwaitingDeviceCount, Complete, Failed }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriveRefreshEvent {
    LayoutWritten,
    VerificationFinished { accepted: bool },
    CountResponse { accepted: bool },
    DriveTable { converged: bool },
    Timeout,
    TransportLost,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriveRefreshEffect {
    Wait,
    QueryDrives,
    Complete,
    Reject,
    IgnoreUnsolicited,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriveRefresh {
    state: DriveRefreshState,
    layout_attempted: bool,
    deadline: Duration,
}
impl Default for DriveRefresh {
    fn default() -> Self {
        Self {
            state: DriveRefreshState::Discovering,
            layout_attempted: false,
            deadline: Duration::from_secs(10),
        }
    }
}
impl DriveRefresh {
    pub fn deadline(&self) -> Duration {
        self.deadline
    }
    pub fn state(&self) -> DriveRefreshState {
        self.state
    }
    pub fn layout_attempted(&self) -> bool {
        self.layout_attempted
    }
    pub fn step(self, event: DriveRefreshEvent) -> Transition<Self, DriveRefreshEffect> {
        self.step_at(event, Duration::ZERO)
    }
    pub fn step_at(
        mut self,
        event: DriveRefreshEvent,
        now: Duration,
    ) -> Transition<Self, DriveRefreshEffect> {
        let event = if now >= self.deadline {
            DriveRefreshEvent::Timeout
        } else {
            event
        };
        let effect = match self.state {
            DriveRefreshState::Complete | DriveRefreshState::Failed => {
                DriveRefreshEffect::IgnoreTerminal
            }
            DriveRefreshState::Discovering | DriveRefreshState::AwaitingDeviceCount => {
                match event {
                    DriveRefreshEvent::VerificationFinished { accepted } => {
                        self.state = if accepted {
                            DriveRefreshState::Complete
                        } else {
                            DriveRefreshState::Failed
                        };
                        if accepted {
                            DriveRefreshEffect::Complete
                        } else {
                            DriveRefreshEffect::Reject
                        }
                    }
                    DriveRefreshEvent::Timeout
                    | DriveRefreshEvent::TransportLost
                    | DriveRefreshEvent::Stop => {
                        self.state = DriveRefreshState::Failed;
                        DriveRefreshEffect::Reject
                    }
                    DriveRefreshEvent::LayoutWritten => {
                        if self.state == DriveRefreshState::Discovering && !self.layout_attempted {
                            self.state = DriveRefreshState::AwaitingDeviceCount;
                            self.layout_attempted = true;
                            DriveRefreshEffect::Wait
                        } else {
                            self.state = DriveRefreshState::Failed;
                            DriveRefreshEffect::Reject
                        }
                    }
                    DriveRefreshEvent::CountResponse { accepted } => {
                        if self.state == DriveRefreshState::Discovering {
                            DriveRefreshEffect::IgnoreUnsolicited
                        } else if accepted {
                            self.state = DriveRefreshState::Discovering;
                            DriveRefreshEffect::QueryDrives
                        } else {
                            self.state = DriveRefreshState::Failed;
                            DriveRefreshEffect::Reject
                        }
                    }
                    DriveRefreshEvent::DriveTable { converged } => {
                        if converged {
                            self.state = DriveRefreshState::Complete;
                            DriveRefreshEffect::Complete
                        } else {
                            DriveRefreshEffect::Wait
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
    fn reconnect_preserves_official_strict_silence_boundary_and_zero_timeout() {
        for check_timeout in [Duration::ZERO, Duration::from_secs(30)] {
            let state = Reconnect::new(Duration::ZERO, check_timeout);
            assert!(!state.due_at(check_timeout));
            assert!(state.due_at(check_timeout + Duration::from_nanos(1)));
            assert!(matches!(
                state
                    .step(ReconnectEvent::Tick { now: check_timeout })
                    .effect,
                ReconnectEffect::Sleep(_)
            ));
        }
    }
    #[test]
    fn reconnect_attempts_have_one_deadline_and_never_confirm_late_success() {
        let transition =
            Reconnect::new(Duration::ZERO, Duration::ZERO).step(ReconnectEvent::Tick {
                now: Duration::from_secs(1),
            });
        let ReconnectState::Attempting { deadline } = transition.state.state() else {
            panic!("not attempting")
        };
        let transition = transition.state.step(ReconnectEvent::AttemptFinished {
            now: deadline - Duration::from_secs(1),
            success: false,
        });
        assert_eq!(
            transition.effect,
            ReconnectEffect::Sleep(Duration::from_secs(1))
        );
        assert_eq!(
            transition.state.state(),
            ReconnectState::Attempting { deadline }
        );
        let transition = transition.state.step(ReconnectEvent::AttemptFinished {
            now: deadline,
            success: true,
        });
        assert_eq!(transition.effect, ReconnectEffect::Fail);
        assert_eq!(
            transition
                .state
                .step(ReconnectEvent::Tick { now: deadline })
                .effect,
            ReconnectEffect::IgnoreTerminal
        );
    }
    #[test]
    fn drive_refresh_covers_every_state_and_event_without_reviving_terminal_work() {
        let events = [
            DriveRefreshEvent::VerificationFinished { accepted: true },
            DriveRefreshEvent::VerificationFinished { accepted: false },
            DriveRefreshEvent::LayoutWritten,
            DriveRefreshEvent::CountResponse { accepted: true },
            DriveRefreshEvent::CountResponse { accepted: false },
            DriveRefreshEvent::DriveTable { converged: true },
            DriveRefreshEvent::DriveTable { converged: false },
            DriveRefreshEvent::Timeout,
            DriveRefreshEvent::TransportLost,
            DriveRefreshEvent::Stop,
        ];
        for &state in DriveRefreshState::ALL {
            for event in events {
                let model = DriveRefresh {
                    state,
                    layout_attempted: state == DriveRefreshState::AwaitingDeviceCount,
                    ..DriveRefresh::default()
                };
                let transition = model.step(event);
                if matches!(
                    state,
                    DriveRefreshState::Complete | DriveRefreshState::Failed
                ) {
                    assert_eq!(transition.state.state(), state);
                    assert_eq!(transition.effect, DriveRefreshEffect::IgnoreTerminal);
                }
            }
        }
    }
    #[test]
    fn unsolicited_counts_do_not_complete_work_and_layout_is_attempted_once() {
        let model = DriveRefresh::default();
        assert_eq!(
            model
                .step(DriveRefreshEvent::CountResponse { accepted: true })
                .effect,
            DriveRefreshEffect::IgnoreUnsolicited
        );
        let pending = model.step(DriveRefreshEvent::LayoutWritten).state;
        assert_eq!(pending.state(), DriveRefreshState::AwaitingDeviceCount);
        let discovering = pending
            .step(DriveRefreshEvent::CountResponse { accepted: true })
            .state;
        assert!(discovering.layout_attempted());
        assert_eq!(
            discovering.step(DriveRefreshEvent::LayoutWritten).effect,
            DriveRefreshEffect::Reject
        );
    }
}

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind { RefreshDrives, Map, Unmap, Storage, Close }
}
impl DriveRefresh {
    /// A pending refresh owns the response stream. Only shutdown can preempt it.
    pub fn admits(&self, command: CommandKind) -> bool {
        match command {
            CommandKind::Close => true,
            CommandKind::RefreshDrives
            | CommandKind::Map
            | CommandKind::Unmap
            | CommandKind::Storage => match self.state {
                DriveRefreshState::Discovering | DriveRefreshState::AwaitingDeviceCount => false,
                DriveRefreshState::Complete | DriveRefreshState::Failed => true,
            },
        }
    }
}
#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn no_command_can_steal_a_pending_refresh_response_stream() {
        for &state in DriveRefreshState::ALL {
            for &command in CommandKind::ALL {
                let model = DriveRefresh {
                    state,
                    layout_attempted: false,
                    ..DriveRefresh::default()
                };
                assert_eq!(
                    model.admits(command),
                    command == CommandKind::Close
                        || matches!(
                            state,
                            DriveRefreshState::Complete | DriveRefreshState::Failed
                        )
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseAction {
    Expected,
    Heartbeat,
    Defer,
    Reject,
}
/// Nested request waiters preserve asynchronous observations for the session
/// owner. Unknown input or a full inbox ends the epoch instead of dropping data.
pub fn response_action(packet_type: u16, expected: u16, deferred: usize) -> ResponseAction {
    if packet_type == expected {
        ResponseAction::Expected
    } else if packet_type == 0x8400 {
        ResponseAction::Heartbeat
    } else if deferred < 32
        && matches!(
            packet_type,
            0x8200 | 0x8240 | 0x8241 | 0x8300 | 0x8410 | 0x8430 | 0x8500 | 0x8530 | 0x8102
        )
    {
        ResponseAction::Defer
    } else {
        ResponseAction::Reject
    }
}
#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn unsolicited_events_are_preserved_with_a_bounded_queue() {
        assert_eq!(
            response_action(0x8410, 0x8410, 32),
            ResponseAction::Expected
        );
        assert_eq!(response_action(0x8200, 0x8410, 31), ResponseAction::Defer);
        assert_eq!(response_action(0x8200, 0x8410, 32), ResponseAction::Reject);
        assert_eq!(response_action(0xffff, 0x8410, 0), ResponseAction::Reject);
        assert_eq!(response_action(0x8110, 0x8410, 0), ResponseAction::Reject);
    }
}

#[cfg(test)]
mod refresh_deadline_tests {
    use super::*;
    #[test]
    fn count_ack_and_partial_tables_never_extend_refresh_or_confirm_after_deadline() {
        let model = DriveRefresh::default()
            .step_at(DriveRefreshEvent::LayoutWritten, Duration::from_secs(8))
            .state;
        let queried = model.step_at(
            DriveRefreshEvent::CountResponse { accepted: true },
            Duration::from_secs(9),
        );
        assert_eq!(queried.effect, DriveRefreshEffect::QueryDrives);
        assert_eq!(queried.state.deadline(), Duration::from_secs(10));
        for event in [
            DriveRefreshEvent::DriveTable { converged: true },
            DriveRefreshEvent::VerificationFinished { accepted: true },
            DriveRefreshEvent::CountResponse { accepted: true },
        ] {
            let late = queried.state.step_at(event, Duration::from_secs(10));
            assert_eq!(late.effect, DriveRefreshEffect::Reject);
            assert_eq!(late.state.state(), DriveRefreshState::Failed);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LivePacketAction {
    Configuration,
    Storage,
    Heartbeat,
    DeviceStatus,
    UnsolicitedRelease,
    Read,
    OperationOwned,
}
/// Read servicing and asynchronous device facts must progress even while a
/// different operation owns an acknowledgement. They are not deferred work.
pub fn live_packet_action(packet_type: u16) -> LivePacketAction {
    match packet_type {
        0x8430 => LivePacketAction::Configuration,
        0x8500 | 0x8530 => LivePacketAction::Storage,
        0x8400 => LivePacketAction::Heartbeat,
        0x8410 => LivePacketAction::DeviceStatus,
        0x8240 => LivePacketAction::UnsolicitedRelease,
        0x8300 => LivePacketAction::Read,
        _ => LivePacketAction::OperationOwned,
    }
}

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadPhase { Ready, AwaitingWrite, Complete, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadFault {
    InvalidGeometry,
    OutsideImage,
    AddressOverflow,
    OutputFailed,
    InvalidPhase,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadEvent {
    Next,
    Written { success: bool },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadEffect {
    Chunk { offset: u32, blocks: u32 },
    Continue,
    Complete,
    Reject(ReadFault),
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadTransfer {
    phase: ReadPhase,
    offset: u32,
    remaining: u32,
    chunk_limit: u32,
    pending: u32,
}
impl ReadTransfer {
    pub fn new(
        offset: u32,
        blocks: u32,
        remote_limit: u32,
        block_size: u32,
        image_size: u64,
    ) -> Result<Self, ReadFault> {
        if blocks == 0 || remote_limit == 0 || block_size == 0 || block_size > 1024 * 1024 - 20 {
            return Err(ReadFault::InvalidGeometry);
        }
        let end = offset
            .checked_add(blocks)
            .ok_or(ReadFault::AddressOverflow)?;
        if u64::from(end) * u64::from(block_size) > image_size {
            return Err(ReadFault::OutsideImage);
        }
        Ok(Self {
            phase: ReadPhase::Ready,
            offset,
            remaining: blocks,
            // One MiB outer limit includes the 12-byte AVMP header and
            // eight bytes of read-response metadata before the image bytes.
            chunk_limit: remote_limit.min((1024 * 1024 - 20) / block_size),
            pending: 0,
        })
    }
    pub fn step(mut self, event: ReadEvent) -> Transition<Self, ReadEffect> {
        let effect = match self.phase {
            ReadPhase::Complete | ReadPhase::Failed => ReadEffect::IgnoreTerminal,
            ReadPhase::Ready | ReadPhase::AwaitingWrite => match event {
                ReadEvent::Next => {
                    if self.phase != ReadPhase::Ready {
                        self.phase = ReadPhase::Failed;
                        ReadEffect::Reject(ReadFault::InvalidPhase)
                    } else {
                        self.pending = self.remaining.min(self.chunk_limit);
                        self.phase = ReadPhase::AwaitingWrite;
                        ReadEffect::Chunk {
                            offset: self.offset,
                            blocks: self.pending,
                        }
                    }
                }
                ReadEvent::Written { success } => {
                    if self.phase != ReadPhase::AwaitingWrite || !success {
                        self.phase = ReadPhase::Failed;
                        ReadEffect::Reject(if success {
                            ReadFault::InvalidPhase
                        } else {
                            ReadFault::OutputFailed
                        })
                    } else {
                        // Constructor admission bounds the entire request before
                        // emitting any data, so neither update can overflow.
                        self.offset += self.pending;
                        self.remaining -= self.pending;
                        if self.remaining == 0 {
                            self.phase = ReadPhase::Complete;
                            ReadEffect::Complete
                        } else {
                            self.phase = ReadPhase::Ready;
                            ReadEffect::Continue
                        }
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
mod read_transfer_tests {
    use super::*;
    #[test]
    fn invalid_and_out_of_image_reads_are_rejected_before_output() {
        for (offset, blocks, limit, size, bytes, fault) in [
            (0, 0, 1, 512, 512, ReadFault::InvalidGeometry),
            (0, 1, 0, 512, 512, ReadFault::InvalidGeometry),
            (0, 1, 1, 0, 512, ReadFault::InvalidGeometry),
            (u32::MAX, 1, 1, 512, u64::MAX, ReadFault::AddressOverflow),
            (1, 1, 1, 512, 512, ReadFault::OutsideImage),
        ] {
            assert_eq!(
                ReadTransfer::new(offset, blocks, limit, size, bytes),
                Err(fault)
            );
        }
    }
    #[test]
    fn remote_maximum_is_an_upper_bound_and_chunks_advance_only_after_output() {
        let model = ReadTransfer::new(0, 513, u32::MAX, 2048, 513 * 2048).unwrap();
        let first = model.step(ReadEvent::Next);
        assert_eq!(
            first.effect,
            ReadEffect::Chunk {
                offset: 0,
                blocks: 511
            }
        );
        let more = first.state.step(ReadEvent::Written { success: true });
        assert_eq!(more.effect, ReadEffect::Continue);
        let last = more.state.step(ReadEvent::Next);
        assert_eq!(
            last.effect,
            ReadEffect::Chunk {
                offset: 511,
                blocks: 2
            }
        );
        assert_eq!(
            last.state.step(ReadEvent::Written { success: true }).effect,
            ReadEffect::Complete
        );
        assert_eq!(
            first
                .state
                .step(ReadEvent::Written { success: false })
                .state
                .step(ReadEvent::Next)
                .effect,
            ReadEffect::IgnoreTerminal
        );
    }
    #[test]
    fn every_read_phase_has_a_policy_for_every_event() {
        for &phase in ReadPhase::ALL {
            for event in [
                ReadEvent::Next,
                ReadEvent::Written { success: true },
                ReadEvent::Written { success: false },
            ] {
                let model = ReadTransfer {
                    phase,
                    offset: 0,
                    remaining: 1,
                    chunk_limit: 1,
                    pending: 1,
                };
                let transition = model.step(event);
                if matches!(phase, ReadPhase::Complete | ReadPhase::Failed) {
                    assert_eq!(transition.state, model);
                }
            }
        }
    }
}

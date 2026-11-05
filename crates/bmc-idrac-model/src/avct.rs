//! Pure iDRAC AVCT display negotiation and recovery decisions.
use crate::Transition;
use alloc::{
    format,
    string::{String, ToString},
};
use core::time::Duration;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SWITCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorDepthProfile {
    pub id: &'static str,
    pub depth_bpp: u8,
    pub variant: bool,
}
const FALLBACK_PROFILE: ColorDepthProfile = ColorDepthProfile {
    id: "variant-1-15",
    depth_bpp: 15,
    variant: true,
};

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DvcStartupPhase {
    /// iDRAC8 retains its official first-display transition unchanged.
    Unmanaged,
    AwaitingSignal,
    AwaitingBootstrap,
    AwaitingDvc15,
    DrainingInitialDvc15Chain,
    AwaitingRefreshedDvc15,
    DecodingRefreshedDvc15Chain,
    Ready,
    Failed,
}
}

impl DvcStartupPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::AwaitingSignal => "awaiting_signal",
            Self::AwaitingBootstrap => "awaiting_bootstrap",
            Self::AwaitingDvc15 => "awaiting_dvc15",
            Self::DrainingInitialDvc15Chain => "draining_initial_dvc15_chain",
            Self::AwaitingRefreshedDvc15 => "awaiting_refreshed_dvc15",
            Self::DecodingRefreshedDvc15Chain => "decoding_refreshed_dvc15_chain",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DvcDisplayAction {
    Passthrough,
    Suppress,
    SuppressAndRequestRefresh,
    DecodeRefreshed { command_chain_boundary: bool },
    RejectUnexpectedFamily,
}

#[derive(Clone, Debug)]
pub struct DvcStartup {
    phase: DvcStartupPhase,
    /// One absolute deadline is retained across every negotiation phase so a
    /// stream that makes partial progress cannot postpone failure forever.
    deadline: Option<Duration>,
}

impl DvcStartup {
    pub fn new(managed: bool, now: Duration) -> Self {
        if managed {
            Self {
                phase: DvcStartupPhase::AwaitingBootstrap,
                deadline: Some(now.saturating_add(STARTUP_TIMEOUT)),
            }
        } else {
            Self {
                phase: DvcStartupPhase::Unmanaged,
                deadline: None,
            }
        }
    }

    fn mark_bootstrap_complete(&mut self) {
        if self.phase == DvcStartupPhase::AwaitingBootstrap {
            self.phase = DvcStartupPhase::AwaitingDvc15;
        }
    }

    fn suspend_for_no_signal(&mut self) {
        if !matches!(
            self.phase,
            DvcStartupPhase::Unmanaged | DvcStartupPhase::Failed
        ) {
            self.phase = DvcStartupPhase::AwaitingSignal;
            self.deadline = None;
        }
    }

    fn resume_for_video_ready(&mut self, now: Duration) {
        if self.phase == DvcStartupPhase::AwaitingSignal {
            self.phase = DvcStartupPhase::AwaitingBootstrap;
            self.deadline = Some(now.saturating_add(STARTUP_TIMEOUT));
        }
    }

    fn observe_display(&mut self, message_type: u16, flags: u8) -> DvcDisplayAction {
        let dvc15 = matches!(message_type, 0x0081 | 0x8601);
        // Official `i#l()` is envelope flag bit one.  When it is set the
        // decoder may not fetch another payload for the current command.
        let command_chain_boundary = flags & 2 != 0;
        // Official flag bit zero resets the retained DVC cursor.  A frame
        // received before this post-refresh reset is still pre-refresh data.
        let resets_cursor = flags & 1 != 0;

        match self.phase {
            DvcStartupPhase::Unmanaged => DvcDisplayAction::Passthrough,
            // Official `m.a(fb)` clears its stopped marker and forwards that
            // same DVC record to the display decoder. A new 0x0084 record is
            // therefore not a prerequisite for recovering from VideoStopped.
            DvcStartupPhase::AwaitingSignal if dvc15 => {
                self.phase = DvcStartupPhase::Ready;
                self.deadline = None;
                DvcDisplayAction::Passthrough
            }
            DvcStartupPhase::AwaitingSignal | DvcStartupPhase::AwaitingBootstrap => {
                DvcDisplayAction::Suppress
            }
            DvcStartupPhase::AwaitingDvc15 if !dvc15 => DvcDisplayAction::Suppress,
            DvcStartupPhase::AwaitingDvc15 => {
                if command_chain_boundary {
                    self.phase = DvcStartupPhase::AwaitingRefreshedDvc15;
                    DvcDisplayAction::SuppressAndRequestRefresh
                } else {
                    self.phase = DvcStartupPhase::DrainingInitialDvc15Chain;
                    DvcDisplayAction::Suppress
                }
            }
            DvcStartupPhase::DrainingInitialDvc15Chain if !dvc15 => {
                self.fail();
                DvcDisplayAction::RejectUnexpectedFamily
            }
            DvcStartupPhase::DrainingInitialDvc15Chain => {
                if command_chain_boundary {
                    self.phase = DvcStartupPhase::AwaitingRefreshedDvc15;
                    DvcDisplayAction::SuppressAndRequestRefresh
                } else {
                    DvcDisplayAction::Suppress
                }
            }
            DvcStartupPhase::AwaitingRefreshedDvc15 if !dvc15 || !resets_cursor => {
                DvcDisplayAction::Suppress
            }
            DvcStartupPhase::AwaitingRefreshedDvc15 => {
                self.phase = DvcStartupPhase::DecodingRefreshedDvc15Chain;
                DvcDisplayAction::DecodeRefreshed {
                    command_chain_boundary,
                }
            }
            DvcStartupPhase::DecodingRefreshedDvc15Chain if !dvc15 || resets_cursor => {
                self.fail();
                DvcDisplayAction::RejectUnexpectedFamily
            }
            DvcStartupPhase::DecodingRefreshedDvc15Chain => DvcDisplayAction::DecodeRefreshed {
                command_chain_boundary,
            },
            // Once a clean DVC15 baseline exists, every source-confirmed DVC
            // family applies to the same retained framebuffer. Profile
            // transitions are bounded separately by DvcProfileSwitch.
            DvcStartupPhase::Ready => DvcDisplayAction::Passthrough,
            DvcStartupPhase::Failed => {
                self.fail();
                DvcDisplayAction::RejectUnexpectedFamily
            }
        }
    }

    fn finish_refreshed_chain(&mut self) -> bool {
        if self.phase != DvcStartupPhase::DecodingRefreshedDvc15Chain {
            return false;
        }
        self.phase = DvcStartupPhase::Ready;
        self.deadline = None;
        true
    }

    fn fail_if_expired(&mut self, now: Duration) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.fail();
            return true;
        }
        false
    }

    fn fail(&mut self) {
        if self.phase != DvcStartupPhase::Unmanaged {
            self.phase = DvcStartupPhase::Failed;
            self.deadline = None;
        }
    }
}

finite_states! {
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DvcProfileSwitchPhase {
    #[default]
    Idle,
    Queued,
    AwaitingTargetTransitionReset,
    DrainingTargetTransition,
    AwaitingTargetReset,
    DecodingTarget,
    AwaitingFallbackTransitionReset,
    DrainingFallbackTransition,
    AwaitingFallbackReset,
    DecodingFallback,
}
}

impl DvcProfileSwitchPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::AwaitingTargetTransitionReset => "awaiting_target_transition_reset",
            Self::DrainingTargetTransition => "draining_target_transition",
            Self::AwaitingTargetReset => "awaiting_target_reset",
            Self::DecodingTarget => "decoding_target",
            Self::AwaitingFallbackTransitionReset => "awaiting_fallback_transition_reset",
            Self::DrainingFallbackTransition => "draining_fallback_transition",
            Self::AwaitingFallbackReset => "awaiting_fallback_reset",
            Self::DecodingFallback => "decoding_fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DvcWireFamily {
    Dvc15,
    Dvc7,
    Gray7,
    Dvc23,
}

impl DvcWireFamily {
    fn from_message_type(message_type: u16) -> Option<Self> {
        match message_type {
            0x0081 | 0x8601 => Some(Self::Dvc15),
            0x0082 | 0x8602 => Some(Self::Dvc7),
            0x0083 | 0x8603 => Some(Self::Gray7),
            0x008a | 0x860a => Some(Self::Dvc23),
            _ => None,
        }
    }

    fn for_profile(profile: ColorDepthProfile) -> Self {
        if !profile.variant {
            Self::Gray7
        } else if profile.depth_bpp == 7 {
            Self::Dvc7
        } else {
            Self::Dvc15
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DvcSwitchAction {
    Passthrough,
    Suppress,
    RequestTargetRefresh,
    DecodeTarget { command_chain_boundary: bool },
    RequestFallbackRefresh,
    DecodeFallback { command_chain_boundary: bool },
    RejectTarget,
    RejectFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DvcSwitchTimeout {
    Target,
    Fallback,
}

#[derive(Clone, Debug)]
pub struct DvcProfileSwitch {
    phase: DvcProfileSwitchPhase,
    generation: u64,
    requested: Option<ColorDepthProfile>,
    chain_message_type: Option<u16>,
    deadline: Option<Duration>,
    failure: Option<String>,
    completion: Option<(u64, Result<(), String>)>,
    active_profile: Option<ColorDepthProfile>,
}

impl Default for DvcProfileSwitch {
    fn default() -> Self {
        Self {
            phase: DvcProfileSwitchPhase::Idle,
            generation: 0,
            requested: None,
            chain_message_type: None,
            deadline: None,
            failure: None,
            completion: None,
            active_profile: None,
        }
    }
}

impl DvcProfileSwitch {
    pub fn phase(&self) -> DvcProfileSwitchPhase {
        self.phase
    }

    fn queue(&mut self, profile: ColorDepthProfile) -> Result<u64, String> {
        if self.phase != DvcProfileSwitchPhase::Idle {
            return Err("another iDRAC DVC color-depth transition is already running".into());
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| "iDRAC DVC operation generation exhausted".to_string())?;
        self.requested = Some(profile);
        self.chain_message_type = None;
        self.deadline = None;
        self.failure = None;
        self.completion = None;
        self.phase = DvcProfileSwitchPhase::Queued;
        Ok(self.generation)
    }

    fn mark_request_written(&mut self, generation: u64, now: Duration) -> bool {
        if self.generation != generation || self.phase != DvcProfileSwitchPhase::Queued {
            return false;
        }
        self.phase = DvcProfileSwitchPhase::AwaitingTargetTransitionReset;
        self.deadline = Some(now.saturating_add(SWITCH_TIMEOUT));
        true
    }

    fn cancel_queued(&mut self, generation: u64) {
        if self.generation == generation && self.phase == DvcProfileSwitchPhase::Queued {
            self.phase = DvcProfileSwitchPhase::Idle;
            self.requested = None;
        }
    }

    fn abort(&mut self, error: &str) -> bool {
        if self.phase == DvcProfileSwitchPhase::Idle {
            return false;
        }
        self.phase = DvcProfileSwitchPhase::Idle;
        self.deadline = None;
        self.chain_message_type = None;
        self.failure = None;
        self.completion = Some((self.generation, Err(error.to_string())));
        true
    }

    fn observe_display(&mut self, message_type: u16, flags: u8) -> DvcSwitchAction {
        let resets_cursor = flags & 1 != 0;
        let command_chain_boundary = flags & 2 != 0;
        let message_family = DvcWireFamily::from_message_type(message_type);
        let requested_family = self.requested.map(DvcWireFamily::for_profile);
        let target_family = message_family.is_some() && message_family == requested_family;
        let dvc15 = message_family == Some(DvcWireFamily::Dvc15);
        match self.phase {
            DvcProfileSwitchPhase::Idle | DvcProfileSwitchPhase::Queued => {
                DvcSwitchAction::Passthrough
            }
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset
                if !target_family || !resets_cursor =>
            {
                DvcSwitchAction::Suppress
            }
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset => {
                self.chain_message_type = Some(message_type);
                if command_chain_boundary {
                    self.phase = DvcProfileSwitchPhase::AwaitingTargetReset;
                    self.chain_message_type = None;
                    DvcSwitchAction::RequestTargetRefresh
                } else {
                    self.phase = DvcProfileSwitchPhase::DrainingTargetTransition;
                    DvcSwitchAction::Suppress
                }
            }
            DvcProfileSwitchPhase::DrainingTargetTransition => {
                if self.chain_message_type != Some(message_type) || resets_cursor {
                    DvcSwitchAction::RejectTarget
                } else if command_chain_boundary {
                    self.phase = DvcProfileSwitchPhase::AwaitingTargetReset;
                    self.chain_message_type = None;
                    DvcSwitchAction::RequestTargetRefresh
                } else {
                    DvcSwitchAction::Suppress
                }
            }
            DvcProfileSwitchPhase::AwaitingTargetReset if !target_family || !resets_cursor => {
                DvcSwitchAction::Suppress
            }
            DvcProfileSwitchPhase::AwaitingTargetReset => {
                self.phase = DvcProfileSwitchPhase::DecodingTarget;
                self.chain_message_type = Some(message_type);
                DvcSwitchAction::DecodeTarget {
                    command_chain_boundary,
                }
            }
            DvcProfileSwitchPhase::DecodingTarget
                if self.chain_message_type == Some(message_type) && !resets_cursor =>
            {
                DvcSwitchAction::DecodeTarget {
                    command_chain_boundary,
                }
            }
            DvcProfileSwitchPhase::DecodingTarget => DvcSwitchAction::RejectTarget,
            DvcProfileSwitchPhase::AwaitingFallbackTransitionReset if !dvc15 || !resets_cursor => {
                DvcSwitchAction::Suppress
            }
            DvcProfileSwitchPhase::AwaitingFallbackTransitionReset => {
                self.chain_message_type = Some(message_type);
                if command_chain_boundary {
                    self.phase = DvcProfileSwitchPhase::AwaitingFallbackReset;
                    self.chain_message_type = None;
                    DvcSwitchAction::RequestFallbackRefresh
                } else {
                    self.phase = DvcProfileSwitchPhase::DrainingFallbackTransition;
                    DvcSwitchAction::Suppress
                }
            }
            DvcProfileSwitchPhase::DrainingFallbackTransition => {
                if !dvc15 || self.chain_message_type != Some(message_type) || resets_cursor {
                    DvcSwitchAction::RejectFallback
                } else if command_chain_boundary {
                    self.phase = DvcProfileSwitchPhase::AwaitingFallbackReset;
                    self.chain_message_type = None;
                    DvcSwitchAction::RequestFallbackRefresh
                } else {
                    DvcSwitchAction::Suppress
                }
            }
            DvcProfileSwitchPhase::AwaitingFallbackReset if !dvc15 || !resets_cursor => {
                DvcSwitchAction::Suppress
            }
            DvcProfileSwitchPhase::AwaitingFallbackReset => {
                self.phase = DvcProfileSwitchPhase::DecodingFallback;
                self.chain_message_type = Some(message_type);
                DvcSwitchAction::DecodeFallback {
                    command_chain_boundary,
                }
            }
            DvcProfileSwitchPhase::DecodingFallback
                if dvc15 && self.chain_message_type == Some(message_type) && !resets_cursor =>
            {
                DvcSwitchAction::DecodeFallback {
                    command_chain_boundary,
                }
            }
            DvcProfileSwitchPhase::DecodingFallback => DvcSwitchAction::RejectFallback,
        }
    }

    fn begin_fallback(&mut self, error: &str, now: Duration) -> bool {
        match self.phase {
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset
            | DvcProfileSwitchPhase::DrainingTargetTransition
            | DvcProfileSwitchPhase::AwaitingTargetReset
            | DvcProfileSwitchPhase::DecodingTarget => {
                self.failure = Some(error.to_string());
                self.chain_message_type = None;
                self.phase = DvcProfileSwitchPhase::AwaitingFallbackTransitionReset;
                self.deadline = Some(now.saturating_add(SWITCH_TIMEOUT));
                true
            }
            DvcProfileSwitchPhase::Idle
            | DvcProfileSwitchPhase::Queued
            | DvcProfileSwitchPhase::AwaitingFallbackTransitionReset
            | DvcProfileSwitchPhase::DrainingFallbackTransition
            | DvcProfileSwitchPhase::AwaitingFallbackReset
            | DvcProfileSwitchPhase::DecodingFallback => false,
        }
    }

    fn finish_target_chain(&mut self) -> bool {
        if self.phase != DvcProfileSwitchPhase::DecodingTarget {
            return false;
        }
        self.active_profile = self.requested;
        self.phase = DvcProfileSwitchPhase::Idle;
        self.deadline = None;
        self.chain_message_type = None;
        self.completion = Some((self.generation, Ok(())));
        true
    }

    fn finish_fallback_chain(&mut self) -> bool {
        if self.phase != DvcProfileSwitchPhase::DecodingFallback {
            return false;
        }
        self.active_profile = Some(FALLBACK_PROFILE);
        self.phase = DvcProfileSwitchPhase::Idle;
        self.deadline = None;
        self.chain_message_type = None;
        let error = self
            .failure
            .take()
            .unwrap_or_else(|| "requested DVC profile failed".to_string());
        self.completion = Some((
            self.generation,
            Err(format!("{error}; restored variant-1-15")),
        ));
        true
    }

    fn take_completion(&mut self, generation: u64) -> Option<Result<(), String>> {
        if self
            .completion
            .as_ref()
            .is_some_and(|(completed, _)| *completed == generation)
        {
            return self.completion.take().map(|(_, result)| result);
        }
        None
    }

    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }

    pub fn is_expired(&self, now: Duration) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    pub fn expired_phase(&self, now: Duration) -> Option<DvcSwitchTimeout> {
        if !self.is_expired(now) {
            return None;
        }
        match self.phase {
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset
            | DvcProfileSwitchPhase::DrainingTargetTransition
            | DvcProfileSwitchPhase::AwaitingTargetReset
            | DvcProfileSwitchPhase::DecodingTarget => Some(DvcSwitchTimeout::Target),
            DvcProfileSwitchPhase::AwaitingFallbackTransitionReset
            | DvcProfileSwitchPhase::DrainingFallbackTransition
            | DvcProfileSwitchPhase::AwaitingFallbackReset
            | DvcProfileSwitchPhase::DecodingFallback => Some(DvcSwitchTimeout::Fallback),
            DvcProfileSwitchPhase::Idle | DvcProfileSwitchPhase::Queued => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum StartupEvent {
    BootstrapWritten,
    NoSignal,
    VideoReady { now: Duration },
    Display { message_type: u16, flags: u8 },
    ChainDecoded,
    Tick { now: Duration },
    Failure,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupEffect {
    None,
    Display(DvcDisplayAction),
    ChainFinished(bool),
    Expired(bool),
}
impl DvcStartup {
    pub fn phase(&self) -> DvcStartupPhase {
        self.phase
    }
    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }
    pub fn step(mut self, event: StartupEvent) -> Transition<Self, StartupEffect> {
        let effect = match event {
            StartupEvent::BootstrapWritten => {
                self.mark_bootstrap_complete();
                StartupEffect::None
            }
            StartupEvent::NoSignal => {
                self.suspend_for_no_signal();
                StartupEffect::None
            }
            StartupEvent::VideoReady { now } => {
                self.resume_for_video_ready(now);
                StartupEffect::None
            }
            StartupEvent::Display {
                message_type,
                flags,
            } => StartupEffect::Display(self.observe_display(message_type, flags)),
            StartupEvent::ChainDecoded => {
                StartupEffect::ChainFinished(self.finish_refreshed_chain())
            }
            StartupEvent::Tick { now } => StartupEffect::Expired(self.fail_if_expired(now)),
            StartupEvent::Failure => {
                self.fail();
                StartupEffect::None
            }
        };
        Transition {
            state: self,
            effect,
        }
    }
}
#[derive(Clone, Debug)]
pub enum SwitchEvent {
    Queue(ColorDepthProfile),
    Written { generation: u64, now: Duration },
    Cancel { generation: u64 },
    Abort(String),
    Display { message_type: u16, flags: u8 },
    Fallback { error: String, now: Duration },
    TargetDecoded,
    FallbackDecoded,
    TakeCompletion { generation: u64 },
    ConfirmProfile(ColorDepthProfile),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwitchEffect {
    ProfileObserved,
    Queued(Result<u64, String>),
    Written(bool),
    Cancelled,
    Aborted(bool),
    Display(DvcSwitchAction),
    FallbackStarted(bool),
    TargetFinished(bool),
    FallbackFinished(bool),
    Completion(Option<Result<(), String>>),
}
impl DvcProfileSwitch {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn requested(&self) -> Option<ColorDepthProfile> {
        self.requested
    }
    pub fn active_profile(&self) -> Option<ColorDepthProfile> {
        self.active_profile
    }
    pub fn step(mut self, event: SwitchEvent) -> Transition<Self, SwitchEffect> {
        let effect = match event {
            SwitchEvent::ConfirmProfile(profile) => {
                self.active_profile = Some(profile);
                SwitchEffect::ProfileObserved
            }
            SwitchEvent::Queue(profile) => SwitchEffect::Queued(self.queue(profile)),
            SwitchEvent::Written { generation, now } => {
                SwitchEffect::Written(self.mark_request_written(generation, now))
            }
            SwitchEvent::Cancel { generation } => {
                self.cancel_queued(generation);
                SwitchEffect::Cancelled
            }
            SwitchEvent::Abort(error) => SwitchEffect::Aborted(self.abort(&error)),
            SwitchEvent::Display {
                message_type,
                flags,
            } => SwitchEffect::Display(self.observe_display(message_type, flags)),
            SwitchEvent::Fallback { error, now } => {
                SwitchEffect::FallbackStarted(self.begin_fallback(&error, now))
            }
            SwitchEvent::TargetDecoded => SwitchEffect::TargetFinished(self.finish_target_chain()),
            SwitchEvent::FallbackDecoded => {
                SwitchEffect::FallbackFinished(self.finish_fallback_chain())
            }
            SwitchEvent::TakeCompletion { generation } => {
                SwitchEffect::Completion(self.take_completion(generation))
            }
        };
        Transition {
            state: self,
            effect,
        }
    }
}

impl Default for DvcStartup {
    fn default() -> Self {
        Self::new(false, Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn startup_state_event_matrix_is_deterministic_and_failure_is_terminal() {
        let events = [
            StartupEvent::BootstrapWritten,
            StartupEvent::NoSignal,
            StartupEvent::VideoReady {
                now: Duration::from_secs(1),
            },
            StartupEvent::Display {
                message_type: 0xffff,
                flags: 0xff,
            },
            StartupEvent::ChainDecoded,
            StartupEvent::Tick {
                now: Duration::from_secs(20),
            },
            StartupEvent::Failure,
        ];
        for &phase in DvcStartupPhase::ALL {
            for event in &events {
                let state = DvcStartup {
                    phase,
                    deadline: if matches!(
                        phase,
                        DvcStartupPhase::Unmanaged
                            | DvcStartupPhase::AwaitingSignal
                            | DvcStartupPhase::Ready
                            | DvcStartupPhase::Failed
                    ) {
                        None
                    } else {
                        Some(Duration::from_secs(10))
                    },
                };
                let first = state.clone().step(event.clone());
                let second = state.step(event.clone());
                assert_eq!(first.state.phase, second.state.phase);
                assert_eq!(first.state.deadline, second.state.deadline);
                assert_eq!(first.effect, second.effect);
                if phase == DvcStartupPhase::Failed {
                    assert_eq!(first.state.phase, DvcStartupPhase::Failed);
                }
            }
        }
    }
    #[test]
    fn startup_deadline_is_absolute_across_partial_progress_and_unknown_packets() {
        let mut state = DvcStartup::new(true, Duration::ZERO);
        state = state.step(StartupEvent::BootstrapWritten).state;
        state = state
            .step(StartupEvent::Display {
                message_type: 0x81,
                flags: 1,
            })
            .state;
        let expired = state.step(StartupEvent::Tick {
            now: Duration::from_secs(10),
        });
        assert_eq!(expired.effect, StartupEffect::Expired(true));
        assert_eq!(expired.state.phase(), DvcStartupPhase::Failed);
    }
    #[test]
    fn switch_state_event_matrix_preserves_deadlines_and_rejects_stale_writes() {
        let events = vec![
            SwitchEvent::Queue(FALLBACK_PROFILE),
            SwitchEvent::Written {
                generation: 1,
                now: Duration::from_secs(1),
            },
            SwitchEvent::Written {
                generation: 99,
                now: Duration::from_secs(1),
            },
            SwitchEvent::Cancel { generation: 99 },
            SwitchEvent::Abort("transport lost".into()),
            SwitchEvent::Display {
                message_type: 0xffff,
                flags: 0xff,
            },
            SwitchEvent::Fallback {
                error: "decode failed".into(),
                now: Duration::from_secs(1),
            },
            SwitchEvent::TargetDecoded,
            SwitchEvent::FallbackDecoded,
            SwitchEvent::TakeCompletion { generation: 99 },
            SwitchEvent::ConfirmProfile(FALLBACK_PROFILE),
        ];
        for &phase in DvcProfileSwitchPhase::ALL {
            for event in &events {
                let state = DvcProfileSwitch {
                    phase,
                    generation: 1,
                    requested: Some(FALLBACK_PROFILE),
                    ..Default::default()
                };
                let first = state.clone().step(event.clone());
                let second = state.step(event.clone());
                assert_eq!(first.state.phase, second.state.phase);
                assert_eq!(first.effect, second.effect);
                if matches!(event, SwitchEvent::Written { generation: 99, .. }) {
                    assert_eq!(first.state.phase, phase);
                    assert_eq!(first.effect, SwitchEffect::Written(false));
                }
            }
        }
    }
    #[test]
    fn only_decoded_reset_marked_target_chain_can_confirm_a_profile() {
        let transition = DvcProfileSwitch::default().step(SwitchEvent::Queue(FALLBACK_PROFILE));
        let SwitchEffect::Queued(Ok(generation)) = transition.effect else {
            panic!("queue failed")
        };
        let mut state = transition
            .state
            .step(SwitchEvent::Written {
                generation,
                now: Duration::ZERO,
            })
            .state;
        for flags in [0, 2] {
            let transition = state.step(SwitchEvent::Display {
                message_type: 0x81,
                flags,
            });
            assert_eq!(
                transition.effect,
                SwitchEffect::Display(DvcSwitchAction::Suppress)
            );
            state = transition.state;
        }
        state = state
            .step(SwitchEvent::Display {
                message_type: 0x81,
                flags: 3,
            })
            .state;
        state = state
            .step(SwitchEvent::Display {
                message_type: 0x81,
                flags: 3,
            })
            .state;
        assert_eq!(state.active_profile(), None);
        state = state.step(SwitchEvent::TargetDecoded).state;
        assert_eq!(state.active_profile(), Some(FALLBACK_PROFILE));
        let result = state.step(SwitchEvent::TakeCompletion { generation });
        assert_eq!(result.effect, SwitchEffect::Completion(Some(Ok(()))));
    }
}

#[cfg(test)]
mod recovery_regressions {
    use super::*;
    #[test]
    fn duplicate_fallback_does_not_extend_its_absolute_deadline() {
        let model = DvcProfileSwitch::default()
            .step(SwitchEvent::Queue(FALLBACK_PROFILE))
            .state;
        let model = model
            .step(SwitchEvent::Written {
                generation: 1,
                now: Duration::ZERO,
            })
            .state;
        let model = model
            .step(SwitchEvent::Fallback {
                error: "target failed".into(),
                now: Duration::from_secs(1),
            })
            .state;
        let deadline = model.deadline();
        let model = model
            .step(SwitchEvent::Fallback {
                error: "late repeated failure".into(),
                now: Duration::from_secs(9),
            })
            .state;
        assert_eq!(model.deadline(), deadline);
        assert_eq!(model.failure.as_deref(), Some("target failed"));
        assert_eq!(
            model.expired_phase(Duration::from_secs(11)),
            Some(DvcSwitchTimeout::Fallback)
        );
    }
    #[test]
    fn exhausted_operation_ids_are_not_reused() {
        let model = DvcProfileSwitch {
            generation: u64::MAX,
            ..Default::default()
        };
        let transition = model.step(SwitchEvent::Queue(FALLBACK_PROFILE));
        assert!(matches!(transition.effect, SwitchEffect::Queued(Err(_))));
        assert_eq!(transition.state.generation(), u64::MAX);
        assert_eq!(transition.state.phase(), DvcProfileSwitchPhase::Idle);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AvctTerminalReason {
    None = 0,
    Shutdown = 1,
    ControlWrite = 2,
    DisplayBootstrapWrite = 3,
    DisplayPacketRejected = 4,
    DisplayAckWrite = 5,
    SocketOrControlQueueClosed = 6,
    UnsupportedVideoPacket = 7,
    SessionTaskPanic = 8,
    Idrac6Dvc15NegotiationFailed = 9,
    Idrac6DvcProfileRecoveryFailed = 10,
    Unknown = 11,
    UnsupportedControlPacket = 12,
}

impl AvctTerminalReason {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::Shutdown,
            2 => Self::ControlWrite,
            3 => Self::DisplayBootstrapWrite,
            4 => Self::DisplayPacketRejected,
            5 => Self::DisplayAckWrite,
            6 => Self::SocketOrControlQueueClosed,
            7 => Self::UnsupportedVideoPacket,
            8 => Self::SessionTaskPanic,
            9 => Self::Idrac6Dvc15NegotiationFailed,
            10 => Self::Idrac6DvcProfileRecoveryFailed,
            12 => Self::UnsupportedControlPacket,
            0 => Self::None,
            _ => Self::Unknown,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedControlPacket => "unsupported_control_packet",
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Shutdown => "shutdown",
            Self::ControlWrite => "control_write",
            Self::DisplayBootstrapWrite => "display_bootstrap_write",
            Self::DisplayPacketRejected => "display_packet_rejected",
            Self::DisplayAckWrite => "display_ack_write",
            Self::SocketOrControlQueueClosed => "socket_or_control_queue_closed",
            Self::UnsupportedVideoPacket => "unsupported_video_packet",
            Self::SessionTaskPanic => "session_task_panic",
            Self::Idrac6Dvc15NegotiationFailed => "idrac6_dvc15_negotiation_failed",
            Self::Idrac6DvcProfileRecoveryFailed => "idrac6_dvc_profile_recovery_failed",
        }
    }
}

/// Only transport failures admit native recovery. Malformed/unsupported device
/// traffic, decode failure, and unknown terminal reasons fail closed.
pub fn native_reconnect_eligible(
    shutting_down: bool,
    handle: u32,
    reason: AvctTerminalReason,
) -> bool {
    let transport_failure = match reason {
        AvctTerminalReason::ControlWrite
        | AvctTerminalReason::DisplayBootstrapWrite
        | AvctTerminalReason::DisplayAckWrite
        | AvctTerminalReason::SocketOrControlQueueClosed => true,
        AvctTerminalReason::None
        | AvctTerminalReason::Shutdown
        | AvctTerminalReason::DisplayPacketRejected
        | AvctTerminalReason::UnsupportedVideoPacket
        | AvctTerminalReason::SessionTaskPanic
        | AvctTerminalReason::Idrac6Dvc15NegotiationFailed
        | AvctTerminalReason::Idrac6DvcProfileRecoveryFailed
        | AvctTerminalReason::Unknown
        | AvctTerminalReason::UnsupportedControlPacket => false,
    };
    !shutting_down && handle != 0 && transport_failure
}

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapPhase { Awaiting, Writing, Ready, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapEvent {
    Requested,
    BaselineNeeded,
    Finished { success: bool },
    NoSignal,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapEffect {
    Write,
    RequestBaseline,
    Wait,
    Ready,
    Reset,
    Reject,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bootstrap {
    phase: BootstrapPhase,
    baseline_requested: bool,
}
impl Default for Bootstrap {
    fn default() -> Self {
        Self {
            phase: BootstrapPhase::Awaiting,
            baseline_requested: false,
        }
    }
}
impl Bootstrap {
    pub fn baseline_requested(&self) -> bool {
        self.baseline_requested
    }
    pub fn phase(&self) -> BootstrapPhase {
        self.phase
    }
    pub fn step(mut self, event: BootstrapEvent) -> Transition<Self, BootstrapEffect> {
        let effect = match self.phase {
            BootstrapPhase::Failed => BootstrapEffect::IgnoreTerminal,
            BootstrapPhase::Awaiting | BootstrapPhase::Writing | BootstrapPhase::Ready => {
                match event {
                    BootstrapEvent::BaselineNeeded => {
                        if self.baseline_requested {
                            BootstrapEffect::Wait
                        } else {
                            self.baseline_requested = true;
                            BootstrapEffect::RequestBaseline
                        }
                    }
                    BootstrapEvent::Requested => {
                        if self.phase == BootstrapPhase::Awaiting {
                            self.phase = BootstrapPhase::Writing;
                            BootstrapEffect::Write
                        } else {
                            BootstrapEffect::Wait
                        }
                    }
                    BootstrapEvent::Finished { success } => {
                        if self.phase == BootstrapPhase::Writing && success {
                            self.phase = BootstrapPhase::Ready;
                            BootstrapEffect::Ready
                        } else {
                            self.phase = BootstrapPhase::Failed;
                            BootstrapEffect::Reject
                        }
                    }
                    BootstrapEvent::NoSignal => {
                        self.phase = BootstrapPhase::Awaiting;
                        BootstrapEffect::Reset
                    }
                    BootstrapEvent::Stop => {
                        self.phase = BootstrapPhase::Failed;
                        BootstrapEffect::Reject
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
mod bootstrap_tests {
    use super::*;
    #[test]
    fn repeated_ready_never_replays_mutation_but_no_signal_requires_fresh_bootstrap() {
        let written = Bootstrap::default()
            .step(BootstrapEvent::Requested)
            .state
            .step(BootstrapEvent::Finished { success: true });
        assert_eq!(
            written.state.step(BootstrapEvent::Requested).effect,
            BootstrapEffect::Wait
        );
        assert_eq!(
            written
                .state
                .step(BootstrapEvent::NoSignal)
                .state
                .step(BootstrapEvent::Requested)
                .effect,
            BootstrapEffect::Write
        );
        for &phase in BootstrapPhase::ALL {
            for event in [
                BootstrapEvent::Requested,
                BootstrapEvent::BaselineNeeded,
                BootstrapEvent::Finished { success: true },
                BootstrapEvent::Finished { success: false },
                BootstrapEvent::NoSignal,
                BootstrapEvent::Stop,
            ] {
                let model = Bootstrap {
                    phase,
                    ..Bootstrap::default()
                };
                let transition = model.step(event);
                if phase == BootstrapPhase::Failed {
                    assert_eq!(transition.state, model);
                }
            }
        }
    }
    #[test]
    fn every_raw_terminal_reason_has_a_reconnect_policy_and_unknown_is_not_success() {
        for raw in 0..=u16::MAX {
            let reason = AvctTerminalReason::from_raw(u32::from(raw));
            if raw > 10 && raw != 12 {
                assert_eq!(reason, AvctTerminalReason::Unknown);
                assert!(!native_reconnect_eligible(false, 1, reason));
            }
            assert!(!native_reconnect_eligible(true, 1, reason));
            assert!(!native_reconnect_eligible(false, 0, reason));
        }
    }
}

/// Queued effects belong to the transport epoch that accepted them.
pub fn command_is_current(current_epoch: u64, command_epoch: u64) -> bool {
    current_epoch == command_epoch
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlAction {
    Observe,
    ColorMode,
    ReconnectHandle,
    RemoteClose,
    Reject,
}
/// Known informational records never authorize a successful operation. Unknown
/// and unimplemented interactive/security records terminate the session.
pub fn control_action(
    message_type: u16,
    payload_len: usize,
    apcp_kind: Option<u16>,
    modern: bool,
) -> ControlAction {
    if let Some(kind) = apcp_kind {
        return match kind {
            0x0400 => ControlAction::Observe,
            0x01ff => ControlAction::RemoteClose,
            _ => ControlAction::Reject,
        };
    }
    match message_type {
        0x8102 if payload_len >= 8 => ControlAction::ReconnectHandle,
        0x8204 if payload_len >= 3 => ControlAction::ColorMode,
        0x8200 | 0x8206 if payload_len >= 8 => ControlAction::Observe,
        0x8201 if payload_len >= 4 => ControlAction::Observe,
        0x8202 if payload_len >= 20 => ControlAction::Observe,
        0x0000 | 0x0001 | 0x0100 | 0x0101 | 0x0201 | 0x0204 | 0x0302 | 0x0304 | 0x030e | 0x0400
        | 0x0600 | 0x0424 | 0x8100 | 0x8101 | 0x8203 | 0x8302 | 0x8430 => ControlAction::Observe,
        0x8410 if modern => ControlAction::Observe,
        _ => ControlAction::Reject,
    }
}
#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn unknown_and_malformed_control_inputs_never_confirm_or_reconnect() {
        for modern in [false, true] {
            for message_type in 0..=u16::MAX {
                let _ = control_action(message_type, 0, None, modern);
            }
            for message_type in [0xffff, 0x9e10, 0x8300, 0x8305, 0x8401] {
                assert_eq!(
                    control_action(message_type, 32, None, modern),
                    ControlAction::Reject
                );
            }
            for (message_type, minimum) in [
                (0x8102, 8),
                (0x8200, 8),
                (0x8201, 4),
                (0x8202, 20),
                (0x8204, 3),
                (0x8206, 8),
            ] {
                assert_eq!(
                    control_action(message_type, minimum - 1, None, modern),
                    ControlAction::Reject
                );
            }
        }
        assert_eq!(
            control_action(0, 8, Some(0x01ff), true),
            ControlAction::RemoteClose
        );
        assert_eq!(
            control_action(0, 8, Some(0x0400), true),
            ControlAction::Observe
        );
        assert_eq!(
            control_action(0, 8, Some(0xffff), true),
            ControlAction::Reject
        );
    }
}

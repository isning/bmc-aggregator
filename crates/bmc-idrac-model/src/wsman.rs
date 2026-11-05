//! WS-MAN response-driven authentication and bounded enumeration.
use crate::Transition;
use alloc::string::String;

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthState { Basic, Digest, Complete, Failed }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthEvent {
    Response(u16),
    TransportFailed,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthEffect {
    RetryDigest,
    Accept,
    Reject,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Authentication {
    state: AuthState,
}
impl Default for Authentication {
    fn default() -> Self {
        Self {
            state: AuthState::Basic,
        }
    }
}
impl Authentication {
    pub fn state(&self) -> AuthState {
        self.state
    }
    pub fn step(self, event: AuthEvent) -> Transition<Self, AuthEffect> {
        let (state, effect) = match self.state {
            AuthState::Complete | AuthState::Failed => (self.state, AuthEffect::IgnoreTerminal),
            AuthState::Basic | AuthState::Digest => match event {
                AuthEvent::TransportFailed | AuthEvent::Stop => {
                    (AuthState::Failed, AuthEffect::Reject)
                }
                AuthEvent::Response(status) => {
                    if status == 401 && self.state == AuthState::Basic {
                        (AuthState::Digest, AuthEffect::RetryDigest)
                    } else if (200..300).contains(&status) {
                        (AuthState::Complete, AuthEffect::Accept)
                    } else {
                        (AuthState::Failed, AuthEffect::Reject)
                    }
                }
            },
        };
        Transition {
            state: Self { state },
            effect,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumerationFault {
    MissingContext,
    PageLimit,
    TransportFailed,
    Cancelled,
}
impl EnumerationFault {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingContext => "has neither context nor end marker",
            Self::PageLimit => "exceeded 16 pages",
            Self::TransportFailed => "transport failed",
            Self::Cancelled => "was cancelled",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumerationState {
    AwaitingPage,
    Complete,
    Failed(EnumerationFault),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumerationEvent {
    Page { end: bool, context: Option<String> },
    TransportFailed,
    Stop,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumerationEffect {
    Pull(String),
    Complete,
    Reject(EnumerationFault),
    IgnoreTerminal,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Enumeration {
    state: EnumerationState,
    pages: u8,
}
impl Default for Enumeration {
    fn default() -> Self {
        Self {
            state: EnumerationState::AwaitingPage,
            pages: 0,
        }
    }
}
impl Enumeration {
    pub fn state(&self) -> EnumerationState {
        self.state
    }
    pub fn pages(&self) -> u8 {
        self.pages
    }
    pub fn step(mut self, event: EnumerationEvent) -> Transition<Self, EnumerationEffect> {
        let effect = match self.state {
            EnumerationState::Complete | EnumerationState::Failed(_) => {
                EnumerationEffect::IgnoreTerminal
            }
            EnumerationState::AwaitingPage => {
                let result = match event {
                    EnumerationEvent::TransportFailed => Err(EnumerationFault::TransportFailed),
                    EnumerationEvent::Stop => Err(EnumerationFault::Cancelled),
                    EnumerationEvent::Page { end, context } => {
                        self.pages += 1;
                        if end {
                            Ok(None)
                        } else if self.pages >= 16 {
                            Err(EnumerationFault::PageLimit)
                        } else {
                            context
                                .filter(|value| !value.trim().is_empty())
                                .map(Some)
                                .ok_or(EnumerationFault::MissingContext)
                        }
                    }
                };
                match result {
                    Ok(None) => {
                        self.state = EnumerationState::Complete;
                        EnumerationEffect::Complete
                    }
                    Ok(Some(context)) => EnumerationEffect::Pull(context),
                    Err(fault) => {
                        self.state = EnumerationState::Failed(fault);
                        EnumerationEffect::Reject(fault)
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
    fn authentication_covers_all_states_and_http_statuses_with_at_most_one_retry() {
        for &state in AuthState::ALL {
            for status in 0..=999 {
                let transition = Authentication { state }.step(AuthEvent::Response(status));
                assert_eq!(
                    transition.effect == AuthEffect::RetryDigest,
                    state == AuthState::Basic && status == 401
                );
                if matches!(state, AuthState::Complete | AuthState::Failed) {
                    assert_eq!(transition.state.state(), state);
                }
            }
            for event in [AuthEvent::TransportFailed, AuthEvent::Stop] {
                let transition = Authentication { state }.step(event);
                assert!(matches!(
                    transition.state.state(),
                    AuthState::Failed | AuthState::Complete
                ));
            }
        }
    }
    #[test]
    fn enumeration_is_bounded_and_does_not_request_a_seventeenth_page() {
        let mut state = Enumeration::default();
        for page in 1..=16 {
            let transition = state.step(EnumerationEvent::Page {
                end: false,
                context: Some("same-context-is-valid".into()),
            });
            if page < 16 {
                assert!(matches!(transition.effect, EnumerationEffect::Pull(_)));
            } else {
                assert_eq!(
                    transition.effect,
                    EnumerationEffect::Reject(EnumerationFault::PageLimit)
                );
            }
            state = transition.state;
        }
        assert_eq!(
            state
                .step(EnumerationEvent::Page {
                    end: true,
                    context: None
                })
                .effect,
            EnumerationEffect::IgnoreTerminal
        );
    }
    #[test]
    fn enumeration_requires_progress_and_accepts_end_on_last_page() {
        for context in [None, Some("".into()), Some("  ".into())] {
            assert_eq!(
                Enumeration::default()
                    .step(EnumerationEvent::Page {
                        end: false,
                        context
                    })
                    .effect,
                EnumerationEffect::Reject(EnumerationFault::MissingContext)
            );
        }
        let state = Enumeration {
            state: EnumerationState::AwaitingPage,
            pages: 15,
        };
        assert_eq!(
            state
                .step(EnumerationEvent::Page {
                    end: true,
                    context: None
                })
                .effect,
            EnumerationEffect::Complete
        );
        for event in [EnumerationEvent::TransportFailed, EnumerationEvent::Stop] {
            let transition = Enumeration::default().step(event);
            assert!(matches!(
                transition.state.state(),
                EnumerationState::Failed(_)
            ));
            assert_eq!(
                transition
                    .state
                    .step(EnumerationEvent::Page {
                        end: true,
                        context: None
                    })
                    .effect,
                EnumerationEffect::IgnoreTerminal
            );
        }
    }
}

#[cfg(test)]
mod enumeration_matrix {
    use super::*;
    #[test]
    fn every_enumeration_state_has_a_deterministic_policy_for_every_event_kind() {
        let phases = [
            EnumerationState::AwaitingPage,
            EnumerationState::Complete,
            EnumerationState::Failed(EnumerationFault::MissingContext),
            EnumerationState::Failed(EnumerationFault::PageLimit),
            EnumerationState::Failed(EnumerationFault::TransportFailed),
            EnumerationState::Failed(EnumerationFault::Cancelled),
        ];
        let events = [
            EnumerationEvent::Page {
                end: true,
                context: None,
            },
            EnumerationEvent::Page {
                end: false,
                context: None,
            },
            EnumerationEvent::Page {
                end: false,
                context: Some("cursor".into()),
            },
            EnumerationEvent::TransportFailed,
            EnumerationEvent::Stop,
        ];
        for state in phases {
            for event in &events {
                let model = Enumeration { state, pages: 1 };
                assert_eq!(model.clone().step(event.clone()), model.step(event.clone()));
            }
        }
    }
}

/// Explicit knowledge about Lifecycle Controller inventory configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryObservation {
    pub current_enabled: Option<bool>,
    pub pending_enabled: Option<bool>,
    pub licensed: Option<bool>,
}
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryStatus { Satisfied, Unavailable, Pending, ActionRequired, Unknown }
}
impl InventoryObservation {
    pub fn status(self) -> InventoryStatus {
        if self.current_enabled == Some(true) {
            InventoryStatus::Satisfied
        } else if self.licensed == Some(false) {
            InventoryStatus::Unavailable
        } else if self.pending_enabled == Some(true) {
            InventoryStatus::Pending
        } else if self.current_enabled == Some(false) {
            InventoryStatus::ActionRequired
        } else {
            InventoryStatus::Unknown
        }
    }
}
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryPhase { Inspecting, Setting, Committing, Verifying, Complete, Unknown, Failed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryEvent {
    Observed(InventoryObservation),
    SetFinished { accepted: bool },
    CommitFinished { accepted: bool },
    TransportLost,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryEffect {
    Set,
    Commit,
    ReadBack,
    NoChange(InventoryStatus),
    Complete(InventoryStatus),
    ReportUnknown,
    Reject,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryRemediation {
    phase: InventoryPhase,
    needs_commit: bool,
}
impl InventoryRemediation {
    pub fn new(needs_commit: bool) -> Self {
        Self {
            phase: InventoryPhase::Inspecting,
            needs_commit,
        }
    }
    pub fn phase(&self) -> InventoryPhase {
        self.phase
    }
    pub fn step(mut self, event: InventoryEvent) -> Transition<Self, InventoryEffect> {
        let effect = match self.phase {
            InventoryPhase::Complete | InventoryPhase::Unknown | InventoryPhase::Failed => {
                InventoryEffect::IgnoreTerminal
            }
            InventoryPhase::Inspecting
            | InventoryPhase::Setting
            | InventoryPhase::Committing
            | InventoryPhase::Verifying => match event {
                InventoryEvent::Stop | InventoryEvent::TransportLost => {
                    self.phase = InventoryPhase::Unknown;
                    InventoryEffect::ReportUnknown
                }
                InventoryEvent::Observed(observation) => {
                    let status = observation.status();
                    match self.phase {
                        InventoryPhase::Inspecting => match status {
                            InventoryStatus::Satisfied
                            | InventoryStatus::Unavailable
                            | InventoryStatus::Pending => {
                                self.phase = InventoryPhase::Complete;
                                InventoryEffect::NoChange(status)
                            }
                            InventoryStatus::ActionRequired => {
                                self.phase = InventoryPhase::Setting;
                                InventoryEffect::Set
                            }
                            InventoryStatus::Unknown => {
                                self.phase = InventoryPhase::Unknown;
                                InventoryEffect::ReportUnknown
                            }
                        },
                        InventoryPhase::Verifying => match status {
                            InventoryStatus::Satisfied | InventoryStatus::Pending => {
                                self.phase = InventoryPhase::Complete;
                                InventoryEffect::Complete(status)
                            }
                            InventoryStatus::Unavailable
                            | InventoryStatus::ActionRequired
                            | InventoryStatus::Unknown => {
                                self.phase = InventoryPhase::Unknown;
                                InventoryEffect::ReportUnknown
                            }
                        },
                        InventoryPhase::Setting
                        | InventoryPhase::Committing
                        | InventoryPhase::Complete
                        | InventoryPhase::Unknown
                        | InventoryPhase::Failed => {
                            self.phase = InventoryPhase::Failed;
                            InventoryEffect::Reject
                        }
                    }
                }
                InventoryEvent::SetFinished { accepted } => {
                    if self.phase != InventoryPhase::Setting || !accepted {
                        self.phase = InventoryPhase::Failed;
                        InventoryEffect::Reject
                    } else if self.needs_commit {
                        self.phase = InventoryPhase::Committing;
                        InventoryEffect::Commit
                    } else {
                        self.phase = InventoryPhase::Verifying;
                        InventoryEffect::ReadBack
                    }
                }
                InventoryEvent::CommitFinished { accepted } => {
                    if self.phase != InventoryPhase::Committing || !accepted {
                        self.phase = InventoryPhase::Failed;
                        InventoryEffect::Reject
                    } else {
                        self.phase = InventoryPhase::Verifying;
                        InventoryEffect::ReadBack
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
mod inventory_tests {
    use super::*;
    #[test]
    fn all_optional_remote_facts_have_an_explicit_classification() {
        let values = [None, Some(false), Some(true)];
        for current_enabled in values {
            for pending_enabled in values {
                for licensed in values {
                    let observation = InventoryObservation {
                        current_enabled,
                        pending_enabled,
                        licensed,
                    };
                    let start =
                        InventoryRemediation::new(true).step(InventoryEvent::Observed(observation));
                    assert_eq!(
                        start.effect == InventoryEffect::Set,
                        observation.status() == InventoryStatus::ActionRequired
                    );
                }
            }
        }
        for status in InventoryStatus::ALL {
            assert!(matches!(
                status,
                InventoryStatus::Satisfied
                    | InventoryStatus::Unavailable
                    | InventoryStatus::Pending
                    | InventoryStatus::ActionRequired
                    | InventoryStatus::Unknown
            ));
        }
    }
    #[test]
    fn matrix_rejects_out_of_order_and_terminal_events_never_replay_mutations() {
        for &phase in InventoryPhase::ALL {
            for event in [
                InventoryEvent::Observed(InventoryObservation {
                    current_enabled: Some(false),
                    pending_enabled: None,
                    licensed: Some(true),
                }),
                InventoryEvent::SetFinished { accepted: true },
                InventoryEvent::SetFinished { accepted: false },
                InventoryEvent::CommitFinished { accepted: true },
                InventoryEvent::CommitFinished { accepted: false },
                InventoryEvent::TransportLost,
                InventoryEvent::Stop,
            ] {
                let model = InventoryRemediation {
                    phase,
                    needs_commit: true,
                };
                let transition = model.step(event);
                assert_eq!(transition, model.step(event));
                if matches!(
                    phase,
                    InventoryPhase::Complete | InventoryPhase::Unknown | InventoryPhase::Failed
                ) {
                    assert_eq!(transition.state, model);
                }
            }
        }
    }
    #[test]
    fn accepted_set_and_job_are_not_success_without_authoritative_readback() {
        let observation = InventoryObservation {
            current_enabled: Some(false),
            pending_enabled: None,
            licensed: Some(true),
        };
        let setting = InventoryRemediation::new(true).step(InventoryEvent::Observed(observation));
        let commit = setting
            .state
            .step(InventoryEvent::SetFinished { accepted: true });
        assert_eq!(commit.effect, InventoryEffect::Commit);
        let readback = commit
            .state
            .step(InventoryEvent::CommitFinished { accepted: true });
        assert_eq!(readback.effect, InventoryEffect::ReadBack);
        assert_eq!(
            readback
                .state
                .step(InventoryEvent::Observed(observation))
                .effect,
            InventoryEffect::ReportUnknown
        );
        assert_eq!(
            readback
                .state
                .step(InventoryEvent::Observed(InventoryObservation {
                    pending_enabled: Some(true),
                    ..observation
                }))
                .effect,
            InventoryEffect::Complete(InventoryStatus::Pending)
        );
    }
}

finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootReadbackPhase { CheckingActivation, CheckingSelection, RecheckingActivation, Confirmed, Unknown }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootReadbackEvent {
    Activation { active: bool },
    Selection { matches: bool },
    Unavailable,
    Stop,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootReadbackEffect {
    QuerySelection,
    QueryActivation,
    Retry,
    Confirmed,
    ReportUnknown,
    IgnoreTerminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootReadback {
    phase: BootReadbackPhase,
}
impl Default for BootReadback {
    fn default() -> Self {
        Self {
            phase: BootReadbackPhase::CheckingActivation,
        }
    }
}
impl BootReadback {
    pub fn step(mut self, event: BootReadbackEvent) -> Transition<Self, BootReadbackEffect> {
        let effect = match self.phase {
            BootReadbackPhase::Confirmed | BootReadbackPhase::Unknown => {
                BootReadbackEffect::IgnoreTerminal
            }
            BootReadbackPhase::CheckingActivation
            | BootReadbackPhase::CheckingSelection
            | BootReadbackPhase::RecheckingActivation => match event {
                BootReadbackEvent::Stop | BootReadbackEvent::Unavailable => {
                    self.phase = BootReadbackPhase::Unknown;
                    BootReadbackEffect::ReportUnknown
                }
                BootReadbackEvent::Activation { active } => match self.phase {
                    BootReadbackPhase::CheckingActivation if active => {
                        self.phase = BootReadbackPhase::CheckingSelection;
                        BootReadbackEffect::QuerySelection
                    }
                    BootReadbackPhase::RecheckingActivation if active => {
                        self.phase = BootReadbackPhase::Confirmed;
                        BootReadbackEffect::Confirmed
                    }
                    BootReadbackPhase::CheckingActivation
                    | BootReadbackPhase::RecheckingActivation => {
                        self.phase = BootReadbackPhase::CheckingActivation;
                        BootReadbackEffect::Retry
                    }
                    BootReadbackPhase::CheckingSelection
                    | BootReadbackPhase::Confirmed
                    | BootReadbackPhase::Unknown => {
                        self.phase = BootReadbackPhase::Unknown;
                        BootReadbackEffect::ReportUnknown
                    }
                },
                BootReadbackEvent::Selection { matches } => {
                    if self.phase != BootReadbackPhase::CheckingSelection {
                        self.phase = BootReadbackPhase::Unknown;
                        BootReadbackEffect::ReportUnknown
                    } else if matches {
                        self.phase = BootReadbackPhase::RecheckingActivation;
                        BootReadbackEffect::QueryActivation
                    } else {
                        self.phase = BootReadbackPhase::CheckingActivation;
                        BootReadbackEffect::Retry
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
mod boot_readback_tests {
    use super::*;
    #[test]
    fn an_entry_read_after_the_host_consumes_one_time_boot_does_not_confirm_selection() {
        let first = BootReadback::default().step(BootReadbackEvent::Activation { active: true });
        assert_eq!(first.effect, BootReadbackEffect::QuerySelection);
        let matching = first
            .state
            .step(BootReadbackEvent::Selection { matches: true });
        assert_eq!(matching.effect, BootReadbackEffect::QueryActivation);
        assert_eq!(
            matching
                .state
                .step(BootReadbackEvent::Activation { active: false })
                .effect,
            BootReadbackEffect::Retry
        );
        assert_eq!(
            matching
                .state
                .step(BootReadbackEvent::Activation { active: true })
                .effect,
            BootReadbackEffect::Confirmed
        );
    }
    #[test]
    fn every_readback_phase_handles_unavailable_out_of_order_and_terminal_observations() {
        for &phase in BootReadbackPhase::ALL {
            for event in [
                BootReadbackEvent::Activation { active: true },
                BootReadbackEvent::Activation { active: false },
                BootReadbackEvent::Selection { matches: true },
                BootReadbackEvent::Selection { matches: false },
                BootReadbackEvent::Unavailable,
                BootReadbackEvent::Stop,
            ] {
                let model = BootReadback { phase };
                let transition = model.step(event);
                assert_eq!(transition, model.step(event));
                if matches!(
                    phase,
                    BootReadbackPhase::Confirmed | BootReadbackPhase::Unknown
                ) {
                    assert_eq!(transition.state, model);
                }
            }
        }
    }
}

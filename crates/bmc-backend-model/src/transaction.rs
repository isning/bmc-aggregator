//! A transport write, protocol acceptance, and observed application are distinct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Sending,
    Dispatched,
    Accepted,
    Confirmed,
    Rejected,
    Unknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Sent,
    Completion(u8),
    Observed(bool),
    Lost,
}
impl State {
    pub fn step(self, event: Event) -> Self {
        if matches!(self, Self::Confirmed | Self::Rejected | Self::Unknown) {
            return self;
        }
        match event {
            Event::Sent => {
                if self == Self::Sending {
                    Self::Dispatched
                } else {
                    self
                }
            }
            Event::Completion(code) => {
                if matches!(self, Self::Sending | Self::Dispatched) {
                    if code == 0 {
                        Self::Accepted
                    } else {
                        Self::Rejected
                    }
                } else {
                    Self::Unknown
                }
            }
            Event::Observed(matches) => {
                if self == Self::Accepted {
                    if matches {
                        Self::Confirmed
                    } else {
                        Self::Unknown
                    }
                } else {
                    Self::Unknown
                }
            }
            Event::Lost => Self::Unknown,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sending_never_confirms_device_state() {
        assert_eq!(State::Sending.step(Event::Sent), State::Dispatched);
        assert_eq!(State::Sending.step(Event::Completion(0)), State::Accepted);
        assert_eq!(
            State::Accepted.step(Event::Observed(true)),
            State::Confirmed
        );
        assert_eq!(
            State::Sending.step(Event::Completion(0xd4)),
            State::Rejected
        );
        assert_eq!(State::Sending.step(Event::Observed(true)), State::Unknown);
    }
    #[test]
    fn terminal_results_do_not_accept_delayed_success() {
        for state in [State::Rejected, State::Unknown, State::Confirmed] {
            for event in [
                Event::Sent,
                Event::Completion(0),
                Event::Observed(true),
                Event::Lost,
            ] {
                assert_eq!(state.step(event), state);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePermission {
    Allowed,
    Denied,
}
impl WritePermission {
    pub fn step(self, completion: u8) -> Self {
        if completion == 0xd4 {
            Self::Denied
        } else {
            self
        }
    }
    pub fn writable(self) -> bool {
        self == Self::Allowed
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    #[test]
    fn denial_is_typed_and_cannot_be_overridden_by_a_late_response() {
        assert_eq!(WritePermission::Allowed.step(0xd4), WritePermission::Denied);
        assert_eq!(WritePermission::Denied.step(0), WritePermission::Denied);
    }
}

#[cfg(test)]
mod completion_matrix {
    use super::*;
    #[test]
    fn every_completion_code_is_explicit_and_only_zero_is_accepted() {
        for code in 0..=255 {
            assert_eq!(
                State::Sending.step(Event::Completion(code)),
                if code == 0 {
                    State::Accepted
                } else {
                    State::Rejected
                }
            );
        }
        for state in [
            State::Sending,
            State::Dispatched,
            State::Accepted,
            State::Confirmed,
            State::Rejected,
            State::Unknown,
        ] {
            for event in [
                Event::Sent,
                Event::Completion(0),
                Event::Completion(1),
                Event::Observed(true),
                Event::Observed(false),
                Event::Lost,
            ] {
                assert_eq!(state.step(event), state.step(event));
                if matches!(state, State::Confirmed | State::Rejected | State::Unknown) {
                    assert_eq!(state.step(event), state);
                }
            }
        }
    }
}

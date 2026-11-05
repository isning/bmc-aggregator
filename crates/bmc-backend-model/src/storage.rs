//! Mahogany storage transport lifecycle and command admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Setup,
    Reply,
    Serving,
    Closed,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Setup(u8),
    Replied,
    Command {
        direction: u8,
        opcode: u8,
        length: u32,
    },
    Fault,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Reply,
    Serve,
    Dispatch,
    Close,
    Reject,
    Ignore,
}
impl State {
    pub fn step(self, event: Event) -> (Self, Action) {
        if matches!(self, Self::Closed | Self::Failed) {
            return (self, Action::Ignore);
        }
        match event {
            Event::Setup(kind) => {
                if self == Self::Setup && kind == 0xfe {
                    (Self::Reply, Action::Reply)
                } else {
                    (Self::Failed, Action::Reject)
                }
            }
            Event::Replied => {
                if self == Self::Reply {
                    (Self::Serving, Action::Serve)
                } else {
                    (Self::Failed, Action::Reject)
                }
            }
            Event::Command {
                direction,
                opcode,
                length,
            } => {
                if self != Self::Serving {
                    (Self::Failed, Action::Reject)
                } else if direction == 0xff || opcode == 0xff {
                    (Self::Closed, Action::Close)
                } else if direction != 0 || length > 16 {
                    (Self::Failed, Action::Reject)
                } else {
                    (self, Action::Dispatch)
                }
            }
            Event::Fault => (Self::Failed, Action::Reject),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_commands_before_setup_and_unknown_directions() {
        let command = Event::Command {
            direction: 0,
            opcode: 0x28,
            length: 10,
        };
        assert_eq!(State::Setup.step(command), (State::Failed, Action::Reject));
        let state = State::Setup
            .step(Event::Setup(0xfe))
            .0
            .step(Event::Replied)
            .0;
        assert_eq!(state.step(command), (State::Serving, Action::Dispatch));
        assert_eq!(
            state.step(Event::Command {
                direction: 1,
                opcode: 0x28,
                length: 10
            }),
            (State::Failed, Action::Reject)
        );
    }
}

#[cfg(test)]
mod transition_matrix {
    use super::*;
    #[test]
    fn every_phase_handles_every_event_kind_including_terminal_duplicates() {
        for state in [
            State::Setup,
            State::Reply,
            State::Serving,
            State::Closed,
            State::Failed,
        ] {
            for event in [
                Event::Setup(0xfe),
                Event::Setup(0),
                Event::Replied,
                Event::Fault,
                Event::Command {
                    direction: 0,
                    opcode: 0x28,
                    length: 10,
                },
                Event::Command {
                    direction: 1,
                    opcode: 0x28,
                    length: 10,
                },
                Event::Command {
                    direction: 0,
                    opcode: 0x28,
                    length: 17,
                },
                Event::Command {
                    direction: 0xff,
                    opcode: 0,
                    length: 0,
                },
            ] {
                let transition = state.step(event);
                assert_eq!(transition, state.step(event));
                if matches!(state, State::Closed | State::Failed) {
                    assert_eq!(transition, (state, Action::Ignore));
                }
                if transition.1 == Action::Dispatch {
                    assert_eq!(state, State::Serving);
                }
                if transition.1 == Action::Reject {
                    assert_eq!(transition.0, State::Failed);
                }
            }
        }
    }
}

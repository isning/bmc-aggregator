//! Pool admission and terminal session decisions. Resource locks remain in the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    Vacant,
    Connecting,
    Online,
    Disconnected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Acquire {
    Connect,
    Wait,
    Lease,
}
pub fn acquire(slot: Slot) -> Acquire {
    match slot {
        Slot::Vacant | Slot::Disconnected => Acquire::Connect,
        Slot::Connecting => Acquire::Wait,
        Slot::Online => Acquire::Lease,
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Session {
    Connecting,
    Online,
    Closed,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Established,
    Fault,
    Stop,
}
impl Session {
    pub fn step(self, event: Event) -> Self {
        if matches!(self, Self::Closed | Self::Failed) {
            return self;
        }
        match event {
            Event::Established => Self::Online,
            Event::Fault => Self::Failed,
            Event::Stop => Self::Closed,
        }
    }
    pub fn accepts_commands(self) -> bool {
        self == Self::Online
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_sessions_cannot_revive() {
        for state in [Session::Closed, Session::Failed] {
            for event in [Event::Established, Event::Fault, Event::Stop] {
                assert_eq!(state.step(event), state);
            }
        }
    }
    #[test]
    fn pool_decisions_cover_every_slot() {
        assert_eq!(acquire(Slot::Vacant), Acquire::Connect);
        assert_eq!(acquire(Slot::Disconnected), Acquire::Connect);
        assert_eq!(acquire(Slot::Connecting), Acquire::Wait);
        assert_eq!(acquire(Slot::Online), Acquire::Lease);
    }
}

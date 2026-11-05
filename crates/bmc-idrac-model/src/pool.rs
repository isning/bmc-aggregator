//! Resource-free shared-console pool decisions. The runtime commits under a lock.
use crate::Transition;
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolState { Vacant, Connecting, Online, Closing, Disconnected, TransportClosed }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolEvent {
    Acquire,
    LastLeaseReleased,
    ConnectFailed,
    ConnectSucceeded,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolEffect {
    Connect { grace: bool },
    WaitForConnect,
    Lease,
    WaitForClose,
    Close,
    Remove,
    Ignore,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pool {
    state: PoolState,
}
impl Pool {
    pub fn new(state: PoolState) -> Self {
        Self { state }
    }
    pub fn state(&self) -> PoolState {
        self.state
    }
    pub fn step(mut self, event: PoolEvent) -> Transition<Self, PoolEffect> {
        let effect = match event {
            PoolEvent::Acquire => match self.state {
                PoolState::Vacant | PoolState::TransportClosed | PoolState::Disconnected => {
                    let grace = self.state == PoolState::Disconnected;
                    self.state = PoolState::Connecting;
                    PoolEffect::Connect { grace }
                }
                PoolState::Connecting => PoolEffect::WaitForConnect,
                PoolState::Online => PoolEffect::Lease,
                PoolState::Closing => PoolEffect::WaitForClose,
            },
            PoolEvent::LastLeaseReleased => match self.state {
                PoolState::Online | PoolState::Disconnected => {
                    self.state = PoolState::Closing;
                    PoolEffect::Close
                }
                PoolState::Vacant
                | PoolState::Connecting
                | PoolState::Closing
                | PoolState::TransportClosed => PoolEffect::Ignore,
            },
            PoolEvent::ConnectFailed => match self.state {
                PoolState::Connecting => {
                    self.state = PoolState::Vacant;
                    PoolEffect::Remove
                }
                PoolState::Vacant
                | PoolState::Online
                | PoolState::Closing
                | PoolState::Disconnected
                | PoolState::TransportClosed => PoolEffect::Ignore,
            },
            PoolEvent::ConnectSucceeded => match self.state {
                PoolState::Connecting => {
                    self.state = PoolState::Online;
                    PoolEffect::Lease
                }
                PoolState::Vacant
                | PoolState::Online
                | PoolState::Closing
                | PoolState::Disconnected
                | PoolState::TransportClosed => PoolEffect::Ignore,
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
    fn all_pool_states_cover_all_events_and_cancelled_connect_releases_waiters() {
        for &state in PoolState::ALL {
            for event in [
                PoolEvent::Acquire,
                PoolEvent::LastLeaseReleased,
                PoolEvent::ConnectFailed,
                PoolEvent::ConnectSucceeded,
            ] {
                let model = Pool::new(state);
                assert_eq!(model.step(event), model.step(event));
            }
        }
        let connecting = Pool::new(PoolState::Vacant).step(PoolEvent::Acquire).state;
        assert_eq!(
            connecting.step(PoolEvent::Acquire).effect,
            PoolEffect::WaitForConnect
        );
        let cancelled = connecting.step(PoolEvent::ConnectFailed);
        assert_eq!(cancelled.effect, PoolEffect::Remove);
        assert_eq!(
            cancelled.state.step(PoolEvent::Acquire).effect,
            PoolEffect::Connect { grace: false }
        );
        assert_eq!(
            Pool::new(PoolState::Closing)
                .step(PoolEvent::Acquire)
                .effect,
            PoolEffect::WaitForClose
        );
    }
}

//! Ordered negotiation, independent of transport and binary encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Greeting,
    Authentication,
    Initialization,
    Online,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    GreetingAccepted,
    Authenticated,
    Initialized,
    Rejected,
}
impl State {
    pub fn step(self, event: Event) -> Self {
        if matches!(self, Self::Online | Self::Failed) {
            return self;
        }
        match event {
            Event::GreetingAccepted => {
                if self == Self::Greeting {
                    Self::Authentication
                } else {
                    Self::Failed
                }
            }
            Event::Authenticated => {
                if self == Self::Authentication {
                    Self::Initialization
                } else {
                    Self::Failed
                }
            }
            Event::Initialized => {
                if self == Self::Initialization {
                    Self::Online
                } else {
                    Self::Failed
                }
            }
            Event::Rejected => Self::Failed,
        }
    }
}
pub fn rfb_minor(major: u16, minor: u16) -> Option<u16> {
    if major != 3 {
        return None;
    }
    if minor >= 8 {
        Some(8)
    } else if minor >= 7 {
        Some(7)
    } else if minor >= 3 {
        Some(3)
    } else {
        None
    }
}
pub fn needs_security_result(minor: u16, password_auth: bool) -> bool {
    password_auth || minor >= 8
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn out_of_order_authentication_fails_closed() {
        assert_eq!(State::Greeting.step(Event::Authenticated), State::Failed);
        assert_eq!(State::Failed.step(Event::Initialized), State::Failed);
        assert!(!needs_security_result(7, false));
        assert!(needs_security_result(8, false));
        assert!(needs_security_result(3, true));
        assert_eq!(rfb_minor(4, 8), None);
    }
}

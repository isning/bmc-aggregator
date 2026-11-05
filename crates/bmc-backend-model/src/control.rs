//! Mahogany keyboard privilege and observed ownership are orthogonal facts.
use core::time::Duration;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Disabled,
    Unknown,
    Shared,
    Granted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    Allow,
    Wait,
    Deny,
    TimedOut,
}
impl Control {
    pub fn new(enabled: bool) -> Self {
        if enabled {
            Self::Unknown
        } else {
            Self::Disabled
        }
    }
    pub fn observe(self, full: bool, can_take: bool) -> (Self, bool) {
        if self == Self::Disabled {
            return (self, false);
        }
        (
            if full { Self::Granted } else { Self::Shared },
            !full && can_take,
        )
    }
    pub fn permission(self, now: Duration, deadline: Duration) -> Permission {
        match self {
            Self::Disabled => Permission::Deny,
            Self::Granted => Permission::Allow,
            Self::Unknown | Self::Shared => {
                if now >= deadline {
                    Permission::TimedOut
                } else {
                    Permission::Wait
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_cannot_override_account_privilege() {
        assert_eq!(
            Control::Disabled.observe(true, true),
            (Control::Disabled, false)
        );
        assert_eq!(
            Control::Unknown.observe(false, true),
            (Control::Shared, true)
        );
        assert_eq!(
            Control::Granted.observe(false, false),
            (Control::Shared, false)
        );
        assert_eq!(
            Control::Unknown.permission(Duration::from_secs(5), Duration::from_secs(5)),
            Permission::TimedOut
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessagePolicy {
    IgnoreMetadata,
    Reject,
}
pub fn unimplemented_message(kind: u8) -> MessagePolicy {
    match kind {
        // Recognized advisory messages with known framing; raw pixels use the negotiated RGBA path.
        0xe4 | 0xe6 | 0xea | 0xeb | 0xec | 0xee => MessagePolicy::IgnoreMetadata,
        _ => MessagePolicy::Reject,
    }
}

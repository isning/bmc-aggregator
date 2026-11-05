//! Redfish task monitoring, with an absolute budget independent of Retry-After.
use core::time::Duration;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Polling,
    Complete,
    Failed,
    Unknown,
    TimedOut,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observation {
    Pending,
    Complete,
    Failed,
    Unknown,
    Tick,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Task {
    state: State,
    deadline: Duration,
}
impl Task {
    pub fn new(deadline: Duration) -> Self {
        Self {
            state: State::Polling,
            deadline,
        }
    }
    pub fn state(self) -> State {
        self.state
    }
    pub fn step(mut self, observation: Observation, now: Duration) -> Self {
        if self.state != State::Polling {
            return self;
        }
        if now >= self.deadline {
            self.state = State::TimedOut;
            return self;
        }
        self.state = match observation {
            Observation::Pending | Observation::Tick => State::Polling,
            Observation::Complete => State::Complete,
            Observation::Failed => State::Failed,
            Observation::Unknown => State::Unknown,
        };
        self
    }
}
pub fn classify(state: Option<&str>, status: Option<&str>, monitor_pending: bool) -> Observation {
    if status == Some("Critical") {
        return Observation::Failed;
    }
    if status.is_some_and(|status| !matches!(status, "OK" | "Warning")) {
        return Observation::Unknown;
    }
    match state {
        Some("Completed") => match status {
            Some("OK" | "Warning") => Observation::Complete,
            Some("Critical") => Observation::Failed,
            Some(_) | None => Observation::Unknown,
        },
        Some("Exception" | "Killed" | "Cancelled" | "Interrupted") => Observation::Failed,
        Some("New" | "Starting" | "Running" | "Suspended" | "Pending" | "Stopping" | "Service") => {
            Observation::Pending
        }
        Some(_) => Observation::Unknown,
        None => {
            if monitor_pending {
                Observation::Pending
            } else {
                Observation::Unknown
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_is_not_success() {
        assert_eq!(
            classify(Some("Completed"), None, false),
            Observation::Unknown
        );
        assert_eq!(
            classify(Some("FutureState"), Some("OK"), true),
            Observation::Unknown
        );
        assert_eq!(classify(None, None, true), Observation::Pending);
        assert_eq!(classify(None, None, false), Observation::Unknown);
    }
    #[test]
    fn deadlines_and_terminal_results_are_absolute() {
        let task = Task::new(Duration::from_secs(30));
        assert_eq!(
            task.step(Observation::Complete, Duration::from_secs(30))
                .state(),
            State::TimedOut
        );
        for observation in [
            Observation::Pending,
            Observation::Complete,
            Observation::Failed,
            Observation::Unknown,
            Observation::Tick,
        ] {
            let terminal = task.step(Observation::Unknown, Duration::ZERO);
            assert_eq!(terminal.step(observation, Duration::ZERO), terminal);
        }
    }
}

#[cfg(test)]
mod transition_matrix {
    use super::*;
    #[test]
    fn every_phase_observation_and_deadline_has_a_policy() {
        for state in [
            State::Polling,
            State::Complete,
            State::Failed,
            State::Unknown,
            State::TimedOut,
        ] {
            for observation in [
                Observation::Pending,
                Observation::Complete,
                Observation::Failed,
                Observation::Unknown,
                Observation::Tick,
            ] {
                for now in [
                    Duration::ZERO,
                    Duration::from_secs(10),
                    Duration::from_secs(11),
                ] {
                    let task = Task {
                        state,
                        deadline: Duration::from_secs(10),
                    };
                    let next = task.step(observation, now);
                    assert_eq!(next, task.step(observation, now));
                    assert_eq!(next.deadline, task.deadline);
                    if state != State::Polling {
                        assert_eq!(next, task);
                    } else if now >= task.deadline {
                        assert_eq!(next.state(), State::TimedOut);
                    }
                }
            }
        }
    }
}

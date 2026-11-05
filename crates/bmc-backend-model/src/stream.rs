//! A partial record has a fixed deadline; more fragments never renew it.
use core::time::Duration;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    Partial,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Record {
    state: State,
    deadline: Option<Duration>,
    budget: Duration,
    limit: usize,
}
impl Record {
    pub fn new(budget: Duration, limit: usize) -> Self {
        Self {
            state: State::Idle,
            deadline: None,
            budget,
            limit,
        }
    }
    pub fn state(self) -> State {
        self.state
    }
    pub fn deadline(self) -> Option<Duration> {
        self.deadline
    }
    pub fn step(mut self, buffered: usize, now: Duration) -> Self {
        if self.state == State::Failed {
            return self;
        }
        if buffered > self.limit || self.deadline.is_some_and(|deadline| now >= deadline) {
            self.state = State::Failed;
        } else if buffered == 0 {
            self.state = State::Idle;
            self.deadline = None;
        } else {
            self.state = State::Partial;
            if self.deadline.is_none() {
                self.deadline = Some(now.saturating_add(self.budget));
            }
        }
        self
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trickled_bytes_cannot_renew_deadline() {
        let record = Record::new(Duration::from_secs(10), 128)
            .step(1, Duration::ZERO)
            .step(2, Duration::from_secs(9));
        assert_eq!(record.deadline(), Some(Duration::from_secs(10)));
        assert_eq!(
            record.step(0, Duration::from_secs(10)).state(),
            State::Failed
        );
        assert_eq!(record.step(0, Duration::from_secs(9)).state(), State::Idle);
    }
}

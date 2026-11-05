//! Bounded traversal, including a terminal page exactly at the limit.
use alloc::{collections::BTreeSet, string::String};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Reading,
    Complete,
    Rejected,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pages {
    state: State,
    visited: BTreeSet<String>,
    limit: usize,
}
impl Pages {
    pub fn new(limit: usize) -> Self {
        Self {
            state: State::Reading,
            visited: BTreeSet::new(),
            limit,
        }
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn enter(mut self, uri: String) -> Self {
        if self.state != State::Reading {
            return self;
        }
        if self.visited.len() >= self.limit || !self.visited.insert(uri) {
            self.state = State::Rejected;
        }
        self
    }
    pub fn finish(mut self, has_next: bool) -> Self {
        if self.state == State::Reading && !has_next {
            self.state = State::Complete;
        }
        self
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_page_at_limit_succeeds_but_loop_and_extra_page_fail() {
        let pages = Pages::new(1).enter(String::from("one"));
        assert_eq!(pages.clone().finish(false).state(), State::Complete);
        assert_eq!(
            pages.clone().enter(String::from("two")).state(),
            State::Rejected
        );
        assert_eq!(pages.enter(String::from("one")).state(), State::Rejected);
    }
}

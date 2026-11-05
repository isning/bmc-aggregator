//! Mutation errors preserve uncertainty across protocol/application boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationFailure {
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationError {
    pub failure: MutationFailure,
    pub detail: String,
}
impl MutationError {
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self {
            failure: MutationFailure::Rejected,
            detail: detail.into(),
        }
    }
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            failure: MutationFailure::Unknown,
            detail: detail.into(),
        }
    }
}
impl From<String> for MutationError {
    fn from(detail: String) -> Self {
        Self::unknown(detail)
    }
}
impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for MutationError {}

impl From<&str> for MutationError {
    fn from(detail: &str) -> Self {
        Self::unknown(detail)
    }
}

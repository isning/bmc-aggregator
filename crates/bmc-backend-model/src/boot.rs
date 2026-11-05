//! Explicit policy for observed Redfish override state and advertised capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Override {
    Unknown,
    Disabled,
    Once,
    Persistent,
}
impl Override {
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("Disabled") => Self::Disabled,
            Some("Once") => Self::Once,
            Some("Continuous") => Self::Persistent,
            Some(_) | None => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_and_unrecognized_override_never_become_one_time_boot() {
        for value in [None, Some(""), Some("VendorExtension"), Some("once")] {
            assert_eq!(Override::from_wire(value), Override::Unknown);
        }
        assert_eq!(Override::from_wire(Some("Disabled")), Override::Disabled);
        assert_eq!(Override::from_wire(Some("Once")), Override::Once);
        assert_eq!(
            Override::from_wire(Some("Continuous")),
            Override::Persistent
        );
    }
}

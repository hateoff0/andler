#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneMode {
    Linked,

    FullStandalone,

    SharedBase,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_mode_variants_are_distinct() {
        assert_ne!(CloneMode::Linked, CloneMode::FullStandalone);
        assert_ne!(CloneMode::Linked, CloneMode::SharedBase);
        assert_ne!(CloneMode::FullStandalone, CloneMode::SharedBase);
    }
}

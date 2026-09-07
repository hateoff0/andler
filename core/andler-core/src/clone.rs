#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneMode {
    Linked,

    FullStandalone,

    SharedBase,
}

impl CloneMode {
    /// Phases of a clone supervisor-operation, with mode-specific weights:
    /// a standalone clone rewrites every byte of the disk, while a linked or
    /// shared-base clone only allocates a thin layer over it.
    pub fn phases(&self) -> Vec<(String, f32)> {
        let disk = match self {
            CloneMode::FullStandalone => 0.9,
            CloneMode::Linked | CloneMode::SharedBase => 0.7,
        };
        let rest = (1.0 - disk) / 2.0;
        vec![
            ("validate".to_string(), rest),
            ("clone-disk".to_string(), disk),
            ("register".to_string(), rest),
        ]
    }
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

    #[test]
    fn clone_phases_sum_to_one_per_mode() {
        for mode in [
            CloneMode::Linked,
            CloneMode::FullStandalone,
            CloneMode::SharedBase,
        ] {
            let phases = mode.phases();
            let total: f32 = phases.iter().map(|(_, weight)| *weight).sum();
            assert!(
                (total - 1.0).abs() < f32::EPSILON,
                "{mode:?} phases summed to {total}"
            );
            let names: Vec<&str> = phases.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(
                names,
                vec!["validate", "clone-disk", "register"],
                "{mode:?} phases"
            );
        }
    }

    #[test]
    fn standalone_clone_weights_the_disk_step_heavier() {
        let standalone = CloneMode::FullStandalone.phases();
        let linked = CloneMode::Linked.phases();
        let disk = |phases: &[(String, f32)]| {
            phases
                .iter()
                .find(|(name, _)| name == "clone-disk")
                .map(|(_, weight)| *weight)
                .unwrap()
        };
        assert!(
            disk(&standalone) > disk(&linked),
            "a standalone clone copies every byte and must out-weight a thin layer"
        );
    }
}

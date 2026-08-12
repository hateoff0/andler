use std::path::{Path, PathBuf};

/// Role of a layer in an instance's disk chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// The active (topmost) layer — the file the running VM writes to.
    Active,
    /// A snapshot layer (external overlay created by `snapshot create`).
    Snapshot,
    /// A layer a linked clone derives from (protected from removal).
    Clone,
    /// The bottom layer of the chain (no backing file).
    BaseImage,
}

/// One layer of a disk chain. `backing` is the layer below this one, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskLayer {
    pub path: PathBuf,
    pub backing: Option<PathBuf>,
    pub kind: LayerKind,
}

/// An instance's disk chain, ordered from the active (head) layer down to the
/// base image. Layer identity is the file path: snapshots rename the previous
/// head into a layer file, so paths are stable for the lifetime of a layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskChain {
    layers: Vec<DiskLayer>,
}

impl DiskChain {
    /// Builds a chain from layers given head-first (active layer first).
    /// Panics when the caller hands out a head-first chain that is not
    /// connected (a layer whose `backing` does not match the next path).
    pub fn from_head_to_base(layers: Vec<DiskLayer>) -> Self {
        for pair in layers.windows(2) {
            debug_assert_eq!(
                pair[0].backing.as_deref(),
                Some(pair[1].path.as_path()),
                "disk chain must be connected head-to-base"
            );
        }
        DiskChain { layers }
    }

    pub fn head(&self) -> Option<&DiskLayer> {
        self.layers.first()
    }

    pub fn layers(&self) -> &[DiskLayer] {
        &self.layers
    }

    pub fn iter(&self) -> impl Iterator<Item = &DiskLayer> {
        self.layers.iter()
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Whether `path` is part of this chain.
    pub fn contains_path(&self, path: &Path) -> bool {
        self.layers.iter().any(|layer| layer.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(path: &str, backing: Option<&str>, kind: LayerKind) -> DiskLayer {
        DiskLayer {
            path: PathBuf::from(path),
            backing: backing.map(PathBuf::from),
            kind,
        }
    }

    #[test]
    fn head_to_base_chain_keeps_order_and_connects() {
        let chain = DiskChain::from_head_to_base(vec![
            layer(
                "disk.qcow2",
                Some("disk.snapshots/s2.qcow2"),
                LayerKind::Active,
            ),
            layer(
                "disk.snapshots/s2.qcow2",
                Some("disk.snapshots/s1.qcow2"),
                LayerKind::Snapshot,
            ),
            layer("disk.snapshots/s1.qcow2", None, LayerKind::BaseImage),
        ]);

        assert_eq!(chain.len(), 3);
        assert_eq!(chain.head().unwrap().path, Path::new("disk.qcow2"));
        let kinds: Vec<LayerKind> = chain.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![LayerKind::Active, LayerKind::Snapshot, LayerKind::BaseImage]
        );
        assert!(chain.contains_path(Path::new("disk.snapshots/s2.qcow2")));
        assert!(!chain.contains_path(Path::new("disk.snapshots/s9.qcow2")));
    }

    #[test]
    fn empty_chain_has_no_head() {
        let chain = DiskChain::default();
        assert!(chain.is_empty());
        assert!(chain.head().is_none());
    }

    #[test]
    fn single_layer_chain_is_connected() {
        let chain =
            DiskChain::from_head_to_base(vec![layer("disk.qcow2", None, LayerKind::Active)]);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain.head().unwrap().backing, None);
    }
}

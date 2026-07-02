//! Auto-detection of `passt` availability. See WIZARD.md, "Auto-detection
//! for passt (network)".

/// Checks the two common install locations for the `passt` binary.
/// Returns `false` (not an error) if neither is present — the wizard
/// falls back to plain SLIRP NAT in that case, it never fails to proceed
/// because of this check.
///
/// Called once by [`super::detect_all`].
pub(crate) fn detect_passt_available() -> bool {
    ["/usr/bin/passt", "/usr/local/bin/passt"]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_bool_without_panicking() {
        // Environment-dependent by design (real filesystem paths) — this
        // just documents that the function is total (never panics) and
        // always returns a plain bool, no matter what's installed on the
        // machine running the test.
        let _: bool = detect_passt_available();
    }
}

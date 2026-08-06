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
        let _: bool = detect_passt_available();
    }
}

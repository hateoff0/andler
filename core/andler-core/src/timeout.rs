//! Wall-clock bounds for the external processes this project spawns.

/// Reads a per-step timeout in seconds from the environment, clamped to a
/// floor.
///
/// Every external process gets one: a hung `guestfish`, `guestmount` or
/// package-manager run used to hang the operation that started it — forever,
/// with nothing in the log to say why, and no way for the caller to tell "this
/// host is slow" from "this will never finish". A value below the floor is
/// read as the floor, because a sub-second bound on an appliance boot is a
/// typo rather than an intent.
pub fn env_secs(var: &str, default: u64, min: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .max(min)
}

#[cfg(test)]
mod tests {
    use super::env_secs;

    /// One test drives the process-wide variable, so the assertions cannot
    /// race each other; the name is unique to this test.
    #[test]
    fn env_secs_defaults_clamps_and_ignores_garbage() {
        std::env::remove_var("ANDLER_TEST_TIMEOUT_SECS");
        assert_eq!(env_secs("ANDLER_TEST_TIMEOUT_SECS", 300, 30), 300);

        std::env::set_var("ANDLER_TEST_TIMEOUT_SECS", "900");
        assert_eq!(env_secs("ANDLER_TEST_TIMEOUT_SECS", 300, 30), 900);

        std::env::set_var("ANDLER_TEST_TIMEOUT_SECS", "5");
        assert_eq!(env_secs("ANDLER_TEST_TIMEOUT_SECS", 300, 30), 30);

        std::env::set_var("ANDLER_TEST_TIMEOUT_SECS", "soon");
        assert_eq!(env_secs("ANDLER_TEST_TIMEOUT_SECS", 300, 30), 300);

        std::env::remove_var("ANDLER_TEST_TIMEOUT_SECS");
    }
}

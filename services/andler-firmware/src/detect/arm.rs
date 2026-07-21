

use andler_core::ArmTranslator;


pub(crate) fn detect_arm_translator() -> Option<ArmTranslator> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    detect_arm_translator_from_cpuinfo(&cpuinfo)
}

fn detect_arm_translator_from_cpuinfo(cpuinfo: &str) -> Option<ArmTranslator> {
    let vendor_line = cpuinfo
        .lines()
        .find(|line| line.starts_with("vendor_id"))?;
    let vendor = vendor_line.split(':').nth(1)?.trim();

    match vendor {
        "AuthenticAMD" => Some(ArmTranslator::Libndk),
        "GenuineIntel" => Some(ArmTranslator::Libhoudini),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amd_vendor_recommends_libndk() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: AuthenticAMD\ncpu family\t: 25\n";
        assert_eq!(
            detect_arm_translator_from_cpuinfo(cpuinfo),
            Some(ArmTranslator::Libndk)
        );
    }

    #[test]
    fn intel_vendor_recommends_libhoudini() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\n";
        assert_eq!(
            detect_arm_translator_from_cpuinfo(cpuinfo),
            Some(ArmTranslator::Libhoudini)
        );
    }

    #[test]
    fn unknown_vendor_recommends_none() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: CentaurHauls\n";
        assert_eq!(detect_arm_translator_from_cpuinfo(cpuinfo), None);
    }

    #[test]
    fn missing_vendor_id_recommends_none() {
        let cpuinfo = "processor\t: 0\ncpu family\t: 6\n";
        assert_eq!(detect_arm_translator_from_cpuinfo(cpuinfo), None);
    }

    #[test]
    fn empty_cpuinfo_recommends_none() {
        assert_eq!(detect_arm_translator_from_cpuinfo(""), None);
    }
}

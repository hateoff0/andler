pub mod houdini;
pub mod ndk;

use andler_core::android_profile::ArmTranslator;

pub struct TranslatorInfo {
    pub dl_links: &'static [(&'static str, &'static str, &'static str)],
    pub files: &'static [&'static str],
    pub props: &'static [(&'static str, &'static str)],
    pub init_rc: Option<&'static str>,
    pub detect_file: &'static str,
}

pub fn resolve(translator: ArmTranslator) -> TranslatorInfo {
    match translator {
        ArmTranslator::Libndk => TranslatorInfo {
            dl_links: ndk::DL_LINKS,
            files: ndk::FILES,
            props: ndk::PROPS,
            init_rc: ndk::INIT_RC,
            detect_file: ndk::DETECT_FILE,
        },
        ArmTranslator::Libhoudini => TranslatorInfo {
            dl_links: houdini::DL_LINKS,
            files: houdini::FILES,
            props: houdini::PROPS,
            init_rc: houdini::INIT_RC,
            detect_file: houdini::DETECT_FILE,
        },
        ArmTranslator::None => TranslatorInfo {
            dl_links: &[],
            files: &[],
            props: &[
                ("ro.product.cpu.abilist", "x86_64,x86"),
                ("ro.product.cpu.abilist32", "x86"),
                ("ro.product.cpu.abilist64", "x86_64"),
                ("ro.dalvik.vm.native.bridge", ""),
                ("ro.enable.native.bridge.exec", "0"),
            ],
            init_rc: None,
            detect_file: "",
        },
    }
}

pub fn dir_name(translator: ArmTranslator) -> &'static str {
    match translator {
        ArmTranslator::Libndk => "ndk",
        ArmTranslator::Libhoudini => "houdini",
        ArmTranslator::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ndk_has_links() {
        let info = resolve(ArmTranslator::Libndk);
        assert!(!info.dl_links.is_empty());
        assert!(!info.files.is_empty());
        assert!(!info.props.is_empty());
        assert!(!info.detect_file.is_empty());
        assert!(info.init_rc.is_none());
    }

    #[test]
    fn resolve_houdini_has_links() {
        let info = resolve(ArmTranslator::Libhoudini);
        assert!(!info.dl_links.is_empty());
        assert!(!info.files.is_empty());
        assert!(!info.props.is_empty());
        assert!(!info.detect_file.is_empty());
        assert!(info.init_rc.is_some());
    }

    #[test]
    fn resolve_none_has_no_links() {
        let info = resolve(ArmTranslator::None);
        assert!(info.dl_links.is_empty());
        assert!(info.files.is_empty());
        assert!(!info.props.is_empty()); // x86-only props
        assert!(info.detect_file.is_empty());
        assert!(info.init_rc.is_none());
    }

    #[test]
    fn dir_names() {
        assert_eq!(dir_name(ArmTranslator::Libndk), "ndk");
        assert_eq!(dir_name(ArmTranslator::Libhoudini), "houdini");
        assert_eq!(dir_name(ArmTranslator::None), "none");
    }
}

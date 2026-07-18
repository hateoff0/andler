//! Constants for Google NDK translation (libndk).
//!
//! Per-translator data module following waydroid_script's separation.
//! Each translator gets its own module with download links, file lists,
//! build.prop patches, and detection paths.

/// Download links for NDK translation archives, keyed by Android version.
/// Format: (android_version, url, expected_md5).
pub const DL_LINKS: &[(&str, &str, &str)] = &[
    (
        "11",
        "https://github.com/supremegamers/vendor_google_proprietary_ndk_translation-prebuilt/archive/9324a8914b649b885dad6f2bfd14a67e5d1520bf.zip",
        "c9572672d1045594448068079b34c350",
    ),
    (
        "13",
        "https://github.com/supremegamers/vendor_google_proprietary_ndk_translation-prebuilt/archive/68734c52556d3d7a6db34c603dd9276915c29f2f.zip",
        "0b2207c490fcb400aa5c87fcf0d52d38",
    ),
];

/// Files to install from the extracted archive into the guest system partition.
pub const FILES: &[&str] = &[
    "bin/arm",
    "bin/arm64",
    "bin/ndk_translation_program_runner_binfmt_misc",
    "etc/init/ndk_translation.rc",
    "lib/arm",
    "lib64/arm64",
    "lib/libndk*",
    "lib64/libndk*",
];

/// build.prop keys to set/update when this translator is active.
pub const PROPS: &[(&str, &str)] = &[
    (
        "ro.product.cpu.abilist",
        "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi",
    ),
    (
        "ro.product.cpu.abilist32",
        "x86,armeabi-v7a,armeabi",
    ),
    ("ro.product.cpu.abilist64", "x86_64,arm64-v8a"),
    ("ro.dalvik.vm.native.bridge", "libndk_translation.so"),
    ("ro.enable.native.bridge.exec", "1"),
    ("ro.vendor.enable.native.bridge.exec", "1"),
    ("ro.vendor.enable.native.bridge.exec64", "1"),
    ("ro.ndk_translation.version", "0.2.3"),
    ("ro.dalvik.vm.isa.arm", "x86"),
    ("ro.dalvik.vm.isa.arm64", "x86_64"),
];

/// Extra init.rc content for this translator (None if not needed).
pub const INIT_RC: Option<&str> = None;

/// File whose presence indicates this translator is installed.
pub const DETECT_FILE: &str = "lib/libndk_translation.so";

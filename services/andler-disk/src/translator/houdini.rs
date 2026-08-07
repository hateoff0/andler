pub const DL_LINKS: &[(&str, &str, &str)] = &[
    (
        "11",
        "https://github.com/supremegamers/vendor_intel_proprietary_houdini/archive/cf7f970f6004f0c329b0464e3d65f9b0e2baea91.zip",
        "5554b11cba905058c3d9bb5e45535d83",
    ),
    (
        "13",
        "https://github.com/supremegamers/vendor_intel_proprietary_houdini/archive/debc3dc91cf12b5c5b8a1c546a5b0b7bf7f838a8.zip",
        "cb7ffac26d47ec7c89df43818e126b47",
    ),
];

pub const FILES: &[&str] = &[
    "bin/arm",
    "bin/arm64",
    "bin/houdini",
    "bin/houdini64",
    "etc/binfmt_misc",
    "etc/init/houdini.rc",
    "lib/arm",
    "lib/libhoudini.so",
    "lib64/arm64",
    "lib64/libhoudini.so",
];

pub const PROPS: &[(&str, &str)] = &[
    (
        "ro.product.cpu.abilist",
        "x86_64,arm64-v8a,x86,armeabi-v7a,armeabi",
    ),
    ("ro.product.cpu.abilist32", "x86,armeabi-v7a,armeabi"),
    ("ro.product.cpu.abilist64", "x86_64,arm64-v8a"),
    ("ro.dalvik.vm.native.bridge", "libhoudini.so"),
    ("ro.enable.native.bridge.exec", "1"),
    ("ro.dalvik.vm.isa.arm", "x86"),
    ("ro.dalvik.vm.isa.arm64", "x86_64"),
];

pub const INIT_RC: Option<&str> = Some(
    r#"on early-init
    mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc

on property:ro.enable.native.bridge.exec=1
    exec -- /system/bin/sh -c "echo ':arm_exe:M::\\\\x7f\\\\x45\\\\x4c\\\\x46\\\\x01\\\\x01\\\\x01\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x02\\\\x00\\\\x28::/system/bin/houdini:P' > /proc/sys/fs/binfmt_misc/register"
    exec -- /system/bin/sh -c "echo ':arm_dyn:M::\\\\x7f\\\\x45\\\\x4c\\\\x46\\\\x01\\\\x01\\\\x01\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x03\\\\x00\\\\x28::/system/bin/houdini:P' >> /proc/sys/fs/binfmt_misc/register"
    exec -- /system/bin/sh -c "echo ':arm64_exe:M::\\\\x7f\\\\x45\\\\x4c\\\\x46\\\\x02\\\\x01\\\\x01\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x02\\\\x00\\\\xb7::/system/bin/houdini64:P' >> /proc/sys/fs/binfmt_misc/register"
    exec -- /system/bin/sh -c "echo ':arm64_dyn:M::\\\\x7f\\\\x45\\\\x4c\\\\x46\\\\x02\\\\x01\\\\x01\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x00\\\\x03\\\\x00\\\\xb7::/system/bin/houdini64:P' >> /proc/sys/fs/binfmt_misc/register"
"#,
);

pub const DETECT_FILE: &str = "lib/libhoudini.so";

pub const DL_LINKS: &[(&str, &str, &str)] = &[
    (
        "11",
        "https://github.com/supremegamers/vendor_intel_proprietary_houdini/archive/81f2a51ef539a35aead396ab7fce2adf89f46e88.zip",
        "fbff756612b4144797fbc99eadcb6653",
    ),
    (
        "13",
        "https://github.com/supremegamers/vendor_intel_proprietary_houdini/archive/9e77896350caccd228b36b2e1b4a994aa4bd48da.zip",
        "3807fe029559db3037efe245d9e74270",
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
        "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi",
    ),
    ("ro.product.cpu.abilist32", "x86,armeabi-v7a,armeabi"),
    ("ro.product.cpu.abilist64", "x86_64,arm64-v8a"),
    ("ro.dalvik.vm.native.bridge", "libhoudini.so"),
    ("ro.enable.native.bridge.exec", "1"),
    ("ro.dalvik.vm.isa.arm", "x86"),
    ("ro.dalvik.vm.isa.arm64", "x86_64"),
];

pub const INIT_RC: Option<&str> = Some(
    "on early-init\n\
     \n\
     on property:ro.enable.native.bridge.exec=1\n\
     \n\
     on property:ro.enable.native.bridge.exec64=1\n",
);

pub const DETECT_FILE: &str = "lib/libhoudini.so";

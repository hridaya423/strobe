fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=src/video/decode.m");
    cc::Build::new()
        .file("src/video/decode.m")
        .flag("-fobjc-arc")
        .flag("-Wno-deprecated-declarations")
        .compile("vrf_decode");
    for framework in ["AVFoundation", "CoreMedia", "CoreVideo", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

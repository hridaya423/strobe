fn main() {
    println!("cargo:rerun-if-changed=src/decode.m");
    cc::Build::new()
        .file("src/decode.m")
        .flag("-fobjc-arc")
        .compile("decode");
    for framework in ["AVFoundation", "CoreMedia", "CoreVideo", "Foundation", "VideoToolbox", "ImageIO", "CoreGraphics", "CoreFoundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

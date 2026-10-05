fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=native/muxer.cpp");
    println!("cargo:rerun-if-changed=native/muxer.h");
    println!("cargo:rerun-if-changed=native/audio.cpp");
    println!("cargo:rerun-if-changed=native/audio.h");
    println!("cargo:rerun-if-changed=native/software_video.cpp");

    println!("cargo:rerun-if-env-changed=STREAMBOX_FFMPEG_INCLUDE_DIR");
    let header_dir = std::env::var("STREAMBOX_FFMPEG_INCLUDE_DIR").ok().or_else(|| {
        std::env::var("STREAMBOX_FFMPEG_LIB_DIR").ok().map(|path| std::path::Path::new(&path).parent().unwrap().join("include").to_string_lossy().into_owned())
    }).or_else(|| match std::env::var("CARGO_CFG_TARGET_ARCH").ok()?.as_str() {
        "arm" => Some("/usr/arm-linux-gnueabihf/include".into()),
        "aarch64" => Some("/usr/aarch64-linux-gnu/include".into()),
        _ => None,
    });
    let mut native = cc::Build::new();
    if let Some(path) = header_dir { native.include(path); }
    // Use headers provided by the same installation as the linked libraries.
    native
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .file("native/muxer.cpp")
        .file("native/audio.cpp")
        .file("native/software_video.cpp")
        .compile("streambox_media");

    println!("cargo:rerun-if-env-changed=STREAMBOX_FFMPEG_LIB_DIR");
    println!("cargo:rerun-if-env-changed=STREAMBOX_STATIC_FFMPEG");
    let lib_dir = std::env::var("STREAMBOX_FFMPEG_LIB_DIR").ok().or_else(|| {
        match std::env::var("CARGO_CFG_TARGET_ARCH").ok()?.as_str() {
            "arm" => Some("/usr/arm-linux-gnueabihf/lib".to_owned()),
            "aarch64" => Some("/usr/aarch64-linux-gnu/lib".to_owned()),
            _ => None,
        }
    });
    if let Some(lib_dir) = lib_dir.filter(|path| std::path::Path::new(path).exists()) {
        println!("cargo:rustc-link-search=native={lib_dir}");
    }
    let link_kind = if std::env::var("STREAMBOX_STATIC_FFMPEG").as_deref() == Ok("1") { "static" } else { "dylib" };
    println!("cargo:rustc-link-lib={link_kind}=avformat");
    println!("cargo:rustc-link-lib={link_kind}=avcodec");
    println!("cargo:rustc-link-lib={link_kind}=swresample");
    println!("cargo:rustc-link-lib={link_kind}=avutil");
    println!("cargo:rustc-link-lib=dylib=asound");
    if link_kind == "static" {
        for library in ["m", "pthread", "rt", "z"] {
            println!("cargo:rustc-link-lib=dylib={library}");
        }
    }
    println!("cargo:rustc-link-lib=dylib=dl");
}

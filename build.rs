fn main() {
    // Link external codec libraries that FFmpeg was built against.
    // CI sets FFMPEG_DEPS_DIR to a prefix containing all static libs (x264, x265).
    // Local dev falls back to homebrew (macOS) or system paths.

    let deps_dir = std::env::var("FFMPEG_DEPS_DIR").ok();

    // x264
    if let Some(ref dir) = deps_dir {
        println!("cargo:rustc-link-search=native={}/lib", dir);
    } else if let Ok(path) = std::env::var("X264_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", path);
    } else {
        // Homebrew (macOS arm64 and x64)
        for path in &["/opt/homebrew/opt/x264/lib", "/usr/local/opt/x264/lib"] {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-search=native={}", path);
                break;
            }
        }
    }
    println!("cargo:rustc-link-lib=static=x264");

    // x265
    if deps_dir.is_none() {
        if let Ok(path) = std::env::var("X265_LIB_DIR") {
            println!("cargo:rustc-link-search=native={}", path);
        } else {
            for path in &["/opt/homebrew/opt/x265/lib", "/usr/local/opt/x265/lib"] {
                if std::path::Path::new(path).exists() {
                    println!("cargo:rustc-link-search=native={}", path);
                    break;
                }
            }
        }
    }
    println!("cargo:rustc-link-lib=static=x265");

    // C++ runtime (x265 dependency)
    let target_os  = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    match target_os.as_str() {
        "macos"   => println!("cargo:rustc-link-lib=c++"),
        "linux"   => println!("cargo:rustc-link-lib=stdc++"),
        "windows" => {
            if target_env == "gnu" {
                println!("cargo:rustc-link-lib=stdc++");
            }
            // msvc: C++ runtime linked automatically
        }
        _ => println!("cargo:rustc-link-lib=stdc++"),
    }

    // Platform frameworks / system libs
    match target_os.as_str() {
        "macos" => {
            for fw in &[
                "AudioToolbox", "AVFoundation", "CoreFoundation", "CoreMedia",
                "CoreServices", "CoreVideo", "Foundation", "Security", "VideoToolbox",
            ] {
                println!("cargo:rustc-link-lib=framework={}", fw);
            }
        }
        "linux" => {
            // pthread, math, dl for FFmpeg on Linux
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
            println!("cargo:rustc-link-lib=dl");
        }
        "windows" => {
            for lib in &[
                "bcrypt", "ole32", "user32", "ws2_32", "secur32", "strmiids",
            ] {
                println!("cargo:rustc-link-lib={}", lib);
            }
        }
        _ => {}
    }

    // zlib
    println!("cargo:rustc-link-lib=z");
}

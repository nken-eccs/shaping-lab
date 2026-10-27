use sha2::{Digest, Sha256};
use std::{env, path::PathBuf, process::Command};

fn hash_files(paths: &[PathBuf]) -> String {
    fn collect(path: &std::path::Path, files: &mut Vec<PathBuf>) {
        if path.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for entry in entries {
                collect(&entry, files);
            }
        } else {
            files.push(path.to_path_buf());
        }
    }
    let mut hasher = Sha256::new();
    for path in paths {
        let mut files = Vec::new();
        collect(path, &mut files);
        for file in files {
            hasher.update(
                file.strip_prefix(path)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .as_bytes(),
            );
            let bytes = std::fs::read(file).unwrap();
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
    format!("{:x}", hasher.finalize())
}

fn main() {
    println!("cargo:rerun-if-env-changed=SHAPING_HARFBUZZ_SOURCE");
    let source = env::var_os("SHAPING_HARFBUZZ_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/harfbuzz"));
    assert!(
        source.join("src/harfbuzz.cc").is_file(),
        "Run python3 lab.py fetch before building"
    );
    println!("cargo:rerun-if-changed={}", source.join("src").display());
    println!(
        "cargo:rustc-env=SHAPING_HB_INCLUDE={}",
        source.join("src").display()
    );
    for path in [
        "src",
        "core",
        "benches",
        "native",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .file(source.join("src/harfbuzz.cc"))
        .file("native/abi-check.cpp")
        .include(source.join("src"))
        .flag_if_supported("-fno-omit-frame-pointer");
    if env::var("PROFILE").as_deref() == Ok("release") {
        build.define("NDEBUG", None);
    }
    let compiler = build.get_compiler();
    println!("cargo:rustc-env=SHAPING_CXX={}", compiler.path().display());
    println!(
        "cargo:rustc-env=SHAPING_CXXFLAGS={}",
        compiler
            .args()
            .iter()
            .map(|v| v.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let compiler_version = compiler
        .to_command()
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).replace('\n', " | "))
        .unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=SHAPING_CXX_VERSION={compiler_version}");
    build.compile("shaping_harfbuzz");
    println!(
        "cargo:rustc-env=SHAPING_HB_SOURCE_SHA256={}",
        hash_files(&[source.join("src")])
    );
    println!(
        "cargo:rustc-env=SHAPING_CODE_SHA256={}",
        hash_files(&[
            "src".into(),
            "core".into(),
            "benches".into(),
            "native".into(),
            "Cargo.toml".into(),
            "build.rs".into()
        ])
    );
    println!(
        "cargo:rustc-env=SHAPING_LOCK_SHA256={:x}",
        Sha256::digest(std::fs::read("Cargo.lock").unwrap())
    );
    let rustc = Command::new(env::var_os("RUSTC").unwrap())
        .arg("-Vv")
        .output()
        .unwrap();
    println!(
        "cargo:rustc-env=SHAPING_RUSTC={}",
        String::from_utf8_lossy(&rustc.stdout).replace('\n', " | ")
    );
    println!(
        "cargo:rustc-env=SHAPING_TARGET={}",
        env::var("TARGET").unwrap()
    );
    println!(
        "cargo:rustc-env=SHAPING_OPT_LEVEL={}",
        env::var("OPT_LEVEL").unwrap()
    );
    println!(
        "cargo:rustc-env=SHAPING_RUSTFLAGS={}",
        env::var("CARGO_ENCODED_RUSTFLAGS")
            .unwrap_or_default()
            .replace('\u{1f}', " ")
    );
}

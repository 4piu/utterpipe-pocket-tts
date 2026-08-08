// Native archive layout and link names are derived from sherpa-onnx 1.13.4,
// Copyright (c) 2022-2026 Next-gen Kaldi contributors, under Apache-2.0.

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use bzip2::read::BzDecoder;
use sha2::{Digest, Sha256};

const STATIC_LIBS: &[&str] = &[
    "sherpa-onnx-c-api",
    "sherpa-onnx-core",
    "kaldi-decoder-core",
    "sherpa-onnx-kaldifst-core",
    "sherpa-onnx-fstfar",
    "sherpa-onnx-fst",
    "kaldi-native-fbank-core",
    "kissfft-float",
    "piper_phonemize",
    "onnxruntime",
    "ssentencepiece_core",
];

fn main() {
    if let Err(error) = run() {
        panic!("failed to prepare the pinned Pocket TTS runtime: {error}");
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=native/espeak_stubs.c");
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_ARCHIVE_DIR");
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let library_dir = if let Some(path) = env::var_os("SHERPA_ONNX_LIB_DIR") {
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(
                format!("SHERPA_ONNX_LIB_DIR is not a directory: {}", path.display()).into(),
            );
        }
        path
    } else {
        verified_archive_library_dir(&target_os, &target_arch)?
    };

    cc::Build::new()
        .cargo_metadata(false)
        .file("native/espeak_stubs.c")
        .warnings(true)
        .compile("sherpa-pocket-espeak-stubs");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    for library in STATIC_LIBS {
        println!("cargo:rustc-link-lib=static={library}");
        if *library == "piper_phonemize" {
            // sherpa-onnx's generic factory retains three eSpeak references,
            // although Pocket never selects that frontend. Resolve them with
            // the fail-closed shim and deliberately do not link libespeak-ng.
            println!("cargo:rustc-link-lib=static=sherpa-pocket-espeak-stubs");
        }
    }
    match target_os.as_str() {
        "linux" => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
            println!("cargo:rustc-link-lib=dylib=m");
            println!("cargo:rustc-link-lib=dylib=pthread");
            println!("cargo:rustc-link-lib=dylib=dl");
        }
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        _ => {}
    }
    Ok(())
}

fn verified_archive_library_dir(
    target_os: &str,
    target_arch: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let (archive_name, expected_hash) = archive_spec(target_os, target_arch)?;
    let archive_root = env::var_os("SHERPA_ONNX_ARCHIVE_DIR")
        .ok_or("set SHERPA_ONNX_ARCHIVE_DIR to the pinned native archive directory")?;
    let archive_path = PathBuf::from(archive_root).join(archive_name);
    if !archive_path.is_file() {
        return Err(format!("native archive is missing: {}", archive_path.display()).into());
    }
    verify_sha256(&archive_path, expected_hash)?;

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let extraction_root = out_dir.join("sherpa-onnx-native");
    let archive_stem = archive_name.trim_end_matches(".tar.bz2");
    let library_dir = extraction_root.join(archive_stem).join("lib");
    if !library_dir.is_dir() {
        if extraction_root.exists() {
            fs::remove_dir_all(&extraction_root)?;
        }
        fs::create_dir_all(&extraction_root)?;
        let archive = File::open(&archive_path)?;
        tar::Archive::new(BzDecoder::new(archive)).unpack(&extraction_root)?;
    }
    if !library_dir.is_dir() {
        return Err(format!(
            "native archive has no expected library directory: {}",
            library_dir.display()
        )
        .into());
    }
    Ok(library_dir)
}

fn archive_spec(
    target_os: &str,
    target_arch: &str,
) -> Result<(&'static str, &'static str), Box<dyn Error>> {
    match (target_os, target_arch) {
        ("macos", "aarch64") => Ok((
            "sherpa-onnx-v1.13.4-osx-arm64-static-lib.tar.bz2",
            "57801db2bbb786a5d343f515a38ff210b401842338bdc804fa075312d1cd2404",
        )),
        ("macos", "x86_64") => Ok((
            "sherpa-onnx-v1.13.4-osx-x64-static-lib.tar.bz2",
            "2bda2c10b31a1cfc45d9f9e14bd4983743ec3779d309e42d99a6c8fa1689043f",
        )),
        ("linux", "x86_64") => Ok((
            "sherpa-onnx-v1.13.4-linux-x64-static-lib.tar.bz2",
            "98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c",
        )),
        ("windows", "x86_64") => Ok((
            "sherpa-onnx-v1.13.4-win-x64-static-MT-Release-lib.tar.bz2",
            "d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c",
        )),
        _ => Err(
            format!("unsupported Pocket runtime target: os={target_os}, arch={target_arch}").into(),
        ),
    }
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(format!(
            "native archive checksum mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )
        .into());
    }
    Ok(())
}

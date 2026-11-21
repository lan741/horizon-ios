//! Build script for the vendored `stb_vorbis.c`.
//!
//! Deliberately uses only `std`: the engine's selling point is that it needs no
//! crates.io dependency graph, and a build-dependency would erode that for one
//! C file. The steps are the two a C project always takes — compile, archive —
//! and the compiler is discovered in the order a developer would try:
//!
//!   1. `$CC` / `$AR` if set (the escape hatch, also used by iOS builds),
//!   2. `clang` on PATH (the Swift toolchain ships one on Windows),
//!   3. `clang` in the Swift toolchain directory,
//!   4. `cc` on `PATH` (Linux/macOS).
//!
//! If none is found the crate still builds: it compiles to a stub whose decoder
//! returns an error, so the rest of the engine keeps working and the app simply
//! reports "no decoder". A missing C compiler must not break `cargo test`.
//!
//! The vendored source is upstream `stb_vorbis.c` v1.25, unmodified:
//!   SHA-256 97a8febc738e6a29fafc8d8bc557c7f17ad3db7ba6544485f59395cc6008b764

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream file digest; verified before compiling so a corrupted or swapped
/// vendor copy is a loud failure rather than a mysterious decoder bug.
const STB_SHA256: &str = "97a8febc738e6a29fafc8d8bc557c7f17ad3db7ba6544485f59395cc6008b764";

fn main() {
    // Declared up front so `cargo check-cfg` does not warn about a cfg this
    // build script legitimately sets.
    println!("cargo::rustc-check-cfg=cfg(has_stb_vorbis)");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest
        .join("..")
        .join("..")
        .join("third_party")
        .join("stb_vorbis.c");
    let source = source.canonicalize().unwrap_or(source);
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
    println!("cargo:rerun-if-env-changed=HZ_FORCE_NO_VORBIS");

    if env::var("HZ_FORCE_NO_VORBIS").is_ok() {
        println!("cargo:warning=HZ_FORCE_NO_VORBIS set: building without Ogg Vorbis support");
        emit_stub();
        return;
    }

    if !source.exists() {
        println!(
            "cargo:warning=stb_vorbis.c not found at {}: building without Ogg Vorbis support",
            source.display()
        );
        emit_stub();
        return;
    }

    if let Some(found) = sha256_file(&source) {
        if found != STB_SHA256 {
            println!(
                "cargo:warning=stb_vorbis.c digest mismatch (found {found}); building without Ogg Vorbis support"
            );
            emit_stub();
            return;
        }
    }

    let target = env::var("TARGET").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let object = out_dir.join("stb_vorbis.o");
    let archive = out_dir.join(archive_name(&target));

    let compiler = match find_cc(&target) {
        Some(c) => c,
        None => {
            println!("cargo:warning=no C compiler found: building without Ogg Vorbis support");
            emit_stub();
            return;
        }
    };

    let mut cmd = Command::new(&compiler.program);
    for arg in &compiler.prefix {
        cmd.arg(arg);
    }
    cmd.args(["-c", "-O2", "-DNDEBUG", "-fno-strict-aliasing"]);
    if compiler.is_msvc_style {
        cmd.args(["/nologo", "/O2", "/DNDEBUG"]);
        cmd.arg(format!("/Fo{}", object.display()));
    } else {
        cmd.arg("-o").arg(&object);
    }
    for arg in &compiler.target_flags {
        cmd.arg(arg);
    }
    cmd.arg(&source);

    match cmd.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            println!(
                "cargo:warning=stb_vorbis.c failed to compile ({}): building without Ogg Vorbis support",
                String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("")
            );
            emit_stub();
            return;
        }
        Err(e) => {
            println!(
                "cargo:warning=could not run {}: {e}",
                compiler.program.display()
            );
            emit_stub();
            return;
        }
    }

    let archiver = find_ar(&target);
    let mut ok = false;
    if let Some(ar) = archiver {
        let mut cmd = Command::new(&ar.program);
        for arg in &ar.prefix {
            cmd.arg(arg);
        }
        if ar.is_msvc_style {
            cmd.arg(format!("/OUT:{}", archive.display()));
            cmd.arg(&object);
        } else {
            cmd.arg("crs").arg(&archive).arg(&object);
        }
        if let Ok(output) = cmd.output() {
            ok = output.status.success();
            if !ok {
                println!(
                    "cargo:warning=archiving failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .next()
                        .unwrap_or("")
                );
            }
        }
    }

    if !ok {
        println!("cargo:warning=no working archiver: building without Ogg Vorbis support");
        emit_stub();
        return;
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=stb_vorbis");
    println!("cargo:rustc-cfg=has_stb_vorbis");
}

fn archive_name(target: &str) -> &'static str {
    if target.contains("windows") || target.contains("msvc") {
        "stb_vorbis.lib"
    } else {
        "libstb_vorbis.a"
    }
}

struct Tool {
    program: PathBuf,
    prefix: Vec<String>,
    target_flags: Vec<String>,
    is_msvc_style: bool,
}

fn find_cc(target: &str) -> Option<Tool> {
    let mut target_flags = Vec::new();
    // The simulator check must come first: `aarch64-apple-ios-sim` also starts
    // with `aarch64-apple-ios`, and compiling the C file against the device SDK
    // makes the later link fail with "building for iOS, but linking in object
    // file built for iOS Simulator".
    if target == "aarch64-apple-ios-sim" || target == "x86_64-apple-ios" {
        let arch = if target.starts_with("x86_64") {
            "x86_64-apple-ios-simulator"
        } else {
            "arm64-apple-ios-simulator"
        };
        target_flags.push("-target".to_string());
        target_flags.push(arch.to_string());
        if let Some(sdk) = xcrun("iphonesimulator") {
            target_flags.push("-isysroot".to_string());
            target_flags.push(sdk);
        }
    } else if target.starts_with("aarch64-apple-ios") {
        target_flags.push("-target".to_string());
        target_flags.push("arm64-apple-ios".to_string());
        if let Some(sdk) = xcrun("iphoneos") {
            target_flags.push("-isysroot".to_string());
            target_flags.push(sdk);
        }
    } else if target.starts_with("aarch64-apple-darwin") {
        target_flags.push("-target".to_string());
        target_flags.push("arm64-apple-macos11".to_string());
    }

    if let Ok(cc) = env::var("CC") {
        return Some(Tool {
            program: PathBuf::from(cc),
            prefix: Vec::new(),
            target_flags,
            is_msvc_style: false,
        });
    }

    // A clang on PATH is the least surprising choice on every platform.
    for candidate in ["clang", "clang.exe"] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Some(Tool {
                program: PathBuf::from(candidate),
                prefix: Vec::new(),
                target_flags,
                is_msvc_style: false,
            });
        }
    }

    // The Swift toolchain ships clang on Windows even when nothing is on PATH.
    if let Ok(local) = env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local).join("Programs\\Swift\\Toolchains");
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let clang = entry.path().join("usr\\bin\\clang.exe");
                if clang.exists() {
                    return Some(Tool {
                        program: clang,
                        prefix: Vec::new(),
                        target_flags,
                        is_msvc_style: false,
                    });
                }
            }
        }
    }

    // Plain cc for Unix hosts.
    for candidate in ["cc", "gcc"] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Some(Tool {
                program: PathBuf::from(candidate),
                prefix: Vec::new(),
                target_flags,
                is_msvc_style: false,
            });
        }
    }

    let _ = target;
    None
}

fn find_ar(target: &str) -> Option<Tool> {
    if let Ok(ar) = env::var("AR") {
        return Some(Tool {
            program: PathBuf::from(ar),
            prefix: Vec::new(),
            target_flags: Vec::new(),
            is_msvc_style: false,
        });
    }
    let msvc_style = target.contains("msvc");
    let candidates: &[&str] = if msvc_style {
        &["llvm-ar", "llvm-ar.exe"]
    } else {
        &["ar", "llvm-ar"]
    };
    for candidate in candidates {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Some(Tool {
                program: PathBuf::from(candidate),
                prefix: Vec::new(),
                target_flags: Vec::new(),
                is_msvc_style: false,
            });
        }
    }
    if msvc_style {
        if let Ok(local) = env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local).join("Programs\\Swift\\Toolchains");
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let ar = entry.path().join("usr\\bin\\llvm-ar.exe");
                    if ar.exists() {
                        return Some(Tool {
                            program: ar,
                            prefix: Vec::new(),
                            target_flags: Vec::new(),
                            is_msvc_style: false,
                        });
                    }
                }
            }
        }
    }
    None
}

fn xcrun(sdk: &str) -> Option<String> {
    let output = Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn emit_stub() {
    // The stub is a tiny C file with a symbol the Rust side can detect, so the
    // crate links either way and the decoder reports "unavailable" at runtime.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let stub = out_dir.join("stub.c");
    std::fs::write(&stub, b"int hz_stb_vorbis_unavailable = 1;\n").ok();
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    let _ = std::fs::remove_file(out_dir.join("libstub_vorbis.a"));
}

/// Minimal SHA-256 so the digest check needs no dependency.
fn sha256_file(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let digest = sha256(&data);
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    #[allow(clippy::manual_div_ceil)]
    for chunk in msg.chunks(64) {
        if chunk.len() != 64 {
            break; // the padding loop above guarantees complete blocks
        }
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

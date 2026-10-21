// Ships `assets/kala-ebpf.bpf.o` — the pre-built kernel-side eBPF
// object — so consumer builds need neither bpf-linker nor a nightly
// toolchain.
//
// Maintainers regenerate the asset with:
//
//     KALA_REBUILD_BPF=1 cargo build
//
// which compiles the standalone crate at `../ebpf` (requires
// `bpf-linker` and the pinned nightly toolchain) and refreshes the
// asset in place. Consumer builds never enter that branch.

use std::{env, fs, path::{Path, PathBuf}, process::Command};

const EBPF_CRATE_REL: &str = "../ebpf";
const EBPF_TARGET: &str = "bpfel-unknown-none";
const EBPF_BIN_NAME: &str = "kala-ebpf-kernel";
const ASSET_DIR_REL: &str = "assets";
const ASSET_NAME: &str = "kala-ebpf.bpf.o";

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let asset = manifest_dir.join(ASSET_DIR_REL).join(ASSET_NAME);

    let rebuild = env::var("KALA_REBUILD_BPF").is_ok() || !asset.exists();
    for path in [EBPF_CRATE_REL, ASSET_DIR_REL] {
        println!("cargo:rerun-if-changed={path}");
    }

    if rebuild {
        if let Err(e) = rebuild_asset(&manifest_dir, &asset) {
            eprintln!("\nerror: failed to build the eBPF object: {e}");
            eprintln!("Install bpf-linker with: cargo install bpf-linker, and a nightly");
            eprintln!("toolchain (or set KALA_REBUILD_BPF off and use a checkout whose");
            eprintln!("published artifact exists).\n");
            std::process::exit(1);
        }
    }
    println!("cargo:rerun-if-changed={}", asset.display());
    println!("cargo:rerun-if-changed={}", asset.display());
}

fn rebuild_asset(manifest_dir: &Path, asset: &PathBuf) -> Result<(), String> {
    let ebpf_dir = manifest_dir.join(EBPF_CRATE_REL);
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let target_dir = out_dir.join("ebpf-target");
    fs::create_dir_all(&target_dir).map_err(|e| format!("create target dir: {e}"))?;

    // Same reasoning as the old build.rs: let `../ebpf/rust-toolchain`
    // (nightly) drive the nested build; clearing RUSTC/RUSTUP_TOOLCHAIN
    // keeps cargo's host-toolchain env from overriding it.
    let host_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let status = Command::new("cargo")
        .arg("build")
        .args(["--target", EBPF_TARGET])
        .arg("--target-dir")
        .arg(&target_dir)
        .args(if host_profile == "release" { &["--release"][..] } else { &[][..] })
        .current_dir(&ebpf_dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTC")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build exited with {status}"));
    }

    let built = target_dir
        .join(EBPF_TARGET)
        .join(if host_profile == "release" { "release" } else { "debug" })
        .join(EBPF_BIN_NAME);
    if !built.exists() {
        return Err(format!("expected eBPF artifact at {}, not found", built.display()));
    }

    fs::create_dir_all(asset.parent().unwrap()).map_err(|e| format!("create assets dir: {e}"))?;
    fs::copy(&built, asset).map_err(|e| format!("copy {built:?} -> {:?}: {e}", asset))?;
    println!("cargo:warning=rebuilt eBPF asset at {}", asset.display());
    Ok(())
}

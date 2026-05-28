// Builds the kernel-side eBPF crate at `../toru-ebpf` and stages its
// output as `$OUT_DIR/toru-ebpf.bpf.o`, which the user-space loader
// then embeds via `include_bytes_aligned!`.
//
// Prerequisite (one-time, per machine):
//
//     cargo install bpf-linker
//
// If `bpf-linker` is missing, the error message tells the developer
// what to do.

use std::{env, fs, path::PathBuf, process::Command};

const EBPF_CRATE_REL: &str = "../toru-ebpf";
const EBPF_TARGET: &str = "bpfel-unknown-none";
const EBPF_BIN_NAME: &str = "toru-ebpf";
const STAGED_OBJECT_NAME: &str = "toru-ebpf.bpf.o";

fn main() {
    if let Err(e) = build_ebpf() {
        eprintln!();
        eprintln!("error: failed to build eBPF crate `{EBPF_CRATE_REL}`: {e}");
        eprintln!();
        eprintln!("If this is the first build on this machine, install bpf-linker:");
        eprintln!("    cargo install bpf-linker");
        eprintln!();
        std::process::exit(1);
    }
}

fn build_ebpf() -> Result<(), String> {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let ebpf_dir = manifest_dir.join(EBPF_CRATE_REL);
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);

    // Watch every source file so cargo knows to re-run on edits.
    println!("cargo:rerun-if-changed={}/src", ebpf_dir.display());
    println!("cargo:rerun-if-changed={}/Cargo.toml", ebpf_dir.display());
    println!("cargo:rerun-if-changed={}/.cargo/config.toml", ebpf_dir.display());
    println!("cargo:rerun-if-changed={}/rust-toolchain", ebpf_dir.display());

    // Profile to use for the eBPF crate: dev when host is dev, release otherwise.
    let host_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let (cargo_profile_arg, profile_dir): (&[&str], &str) = if host_profile == "release" {
        (&["--release"], "release")
    } else {
        (&[], "debug")
    };

    let target_dir = out_dir.join("ebpf-target");
    fs::create_dir_all(&target_dir).map_err(|e| format!("create target dir: {e}"))?;

    // Invoke cargo. `--target` is set; the ebpf crate's own
    // `.cargo/config.toml` also adds `-Z build-std=core`, so we don't
    // need to repeat that here.
    //
    // RUSTUP_TOOLCHAIN is cleared so the ebpf crate's `rust-toolchain`
    // file (nightly) is honoured even when the parent build is on
    // stable. CARGO_TARGET_DIR / RUSTFLAGS are cleared so host-target
    // flags don't leak into bpf-linker.
    let status = Command::new("cargo")
        .arg("build")
        .args(["--target", EBPF_TARGET])
        .arg("--target-dir")
        .arg(&target_dir)
        .args(cargo_profile_arg)
        .current_dir(&ebpf_dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build exited with {status}"));
    }

    // The built artifact is the bin's ELF .o (in eBPF land cargo emits
    // an ELF file with the bin name and no extension). Stage it into
    // OUT_DIR under a stable name.
    let built = target_dir
        .join(EBPF_TARGET)
        .join(profile_dir)
        .join(EBPF_BIN_NAME);
    if !built.exists() {
        return Err(format!(
            "expected eBPF artifact at {} but it doesn't exist",
            built.display()
        ));
    }

    let staged = out_dir.join(STAGED_OBJECT_NAME);
    fs::copy(&built, &staged)
        .map_err(|e| format!("copy {} -> {}: {e}", built.display(), staged.display()))?;

    Ok(())
}

# kala-ebpf

[![Crates.io](https://img.shields.io/crates/v/kala-ebpf?style=flat-square&logo=rust)](https://crates.io/crates/kala-ebpf)
[![docs.rs](https://img.shields.io/docsrs/kala-ebpf?style=flat-square&logo=docsdotrs)](https://docs.rs/kala-ebpf)
[![CI](https://img.shields.io/github/actions/workflow/status/pavelc4/kala/ci.yml?style=flat-square&label=CI)](https://github.com/pavelc4/kala/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](#license)

Per-app frame-time probe for Android, powered by an eBPF uprobe on
libgui's `Surface::queueBuffer`. Drop it into any tool that needs
real, per-frame timing for a target app — no copying BPF programs or
ring-buffer plumbing required.

```rust
use kala_ebpf::FrameProbe;

let mut probe = FrameProbe::new()?;
probe.attach(1234)?; // PID of the target app

while let Some((pid, frametime)) =
    probe.recv_with_deadline(Duration::from_millis(50))
{
    println!("pid {pid}: {:.2} ms", frametime.as_secs_f64() * 1e3);
}
```

## Requirements

**At runtime**

* Linux kernel with eBPF uprobe support and BPF ring-buffer
  (kernel 5.8+; tested on 5.10).
* Root / `CAP_SYS_ADMIN` + `CAP_BPF`.
* `/system/lib64/libgui.so` present (a real Android image, not a
  desktop Linux box).

**At build time (maintainers only)**

* `bpf-linker` on the host: `cargo install bpf-linker`.
* A nightly toolchain (only used to build the BPF crate; the
  `kala-ebpf` crate itself builds on stable).
* The published crate carries the pre-built BPF object (produced by
  CI or `KALA_REBUILD_BPF=1 cargo build`), embedded via
  `include_bytes_aligned!`, so downstream consumers do **not** need
  `bpf-linker` or nightly. Fresh git checkouts compile `ebpf/`
  automatically at build time.

## Compatibility

AOSP churns the `Surface::queueBuffer` signature. The probe walks an
ordered fallback list of mangled symbols — `hook_queueBuffer` (the
C-API trampoline, stable since day one) first, then
`queueBufferInternal`, then the historical overloads. Verified
available on Android 17 (`libgui.so`, API 37).

## How it works

`Surface::queueBuffer` is called by every Android frame producer (NDK,
OpenGL, Vulkan, system UI) right before a frame is handed to
SurfaceFlinger. We attach a uprobe to it, capture the buffer pointer
(used as a per-back-buffer key) and the kernel monotonic timestamp,
then submit a `FrameRecord` to a per-PID ring buffer.

User-space polls each ring buffer with `mio`, groups records by buffer
pointer to undo triple-buffering interleave, and yields one frametime
per displayed frame.

## License

Licensed under the [MIT license](LICENSE).

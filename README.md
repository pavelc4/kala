<p align="center">
    <a href="https://crates.io/crates/toru">
        <img src="https://img.shields.io/crates/v/toru?style=for-the-badge&colorA=363A4F&logo=rust&logoColor=D9E0EE" alt="crates.io">
    </a>
    <a href="https://docs.rs/toru">
        <img src="https://img.shields.io/docsrs/toru?style=for-the-badge&colorA=363A4F&logo=docsdotrs&logoColor=D9E0EE" alt="docs.rs">
    </a>
    <a href="#license">
        <img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=for-the-badge&colorA=363A4F" alt="license">
    </a>
</p>

Per-app frame-time probe for Android, powered by an eBPF uprobe on
libgui's `Surface::queueBuffer`. Drop it into any tool that needs
real, per-frame timing for a target app — no copying BPF programs or
ring-buffer plumbing required.

```rust
use std::time::Duration;
use toru::FrameProbe;

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

**At build time**

* `bpf-linker` on the host: `cargo install bpf-linker`.
* A nightly toolchain (only used to build the BPF crate; the `toru`
  crate itself builds on stable).

**Downstream consumers** (people who depend on `toru` from crates.io)
do **not** need `bpf-linker` or nightly — the BPF object is
pre-built and embedded into the published library via
`include_bytes_aligned!`.

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

Licensed under either of:

* [Apache License, Version 2.0](LICENSE-APACHE)
* [MIT license](LICENSE-MIT)


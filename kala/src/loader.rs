//! Loads the embedded eBPF object and bumps the memlock rlimit.
//!
//! The rlimit bump is needed on kernels <5.11 that account eBPF maps
//! against `RLIMIT_MEMLOCK` (post-memcg accounting it's a no-op). We
//! do it lazily through `OnceLock` instead of a `#[ctor]` static so
//! the work is only done if a probe is actually started.

use std::sync::OnceLock;

use aya::{Ebpf, include_bytes_aligned};

use crate::error::Result;

static MEMLOCK_BUMPED: OnceLock<()> = OnceLock::new();

/// Build a fresh [`Ebpf`] instance from the bytes embedded at compile
/// time. Each attach gets its own instance so the kernel can attribute
/// uprobe events to the right ring buffer without us doing per-task
/// keying inside the program.
pub fn load() -> Result<Ebpf> {
    MEMLOCK_BUMPED.get_or_init(bump_memlock);

    // The pre-built BPF object ships inside the crate as an asset, so
    // consumers don't need bpf-linker. Maintainers regenerate it with
    // KALA_REBUILD_BPF=1 (see build.rs).
    let bpf = Ebpf::load(include_bytes_aligned!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/kala-ebpf.bpf.o"
    )))?;
    Ok(bpf)
}

/// Raise `RLIMIT_MEMLOCK` to infinity. The call may fail (e.g. without
/// `CAP_SYS_RESOURCE`) — we ignore the result because newer kernels
/// don't need it anyway, and on the kernels that do, the subsequent
/// `Ebpf::load` will produce a clearer error message.
fn bump_memlock() {
    let unlimited = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: `unlimited` is a properly initialised rlimit; setrlimit
    // takes a pointer to one. The kernel will either accept it or
    // return -1, both safe outcomes.
    unsafe {
        libc::setrlimit(libc::RLIMIT_MEMLOCK, &raw const unlimited);
    }
}

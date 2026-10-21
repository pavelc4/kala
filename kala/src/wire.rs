//! Wire format shared between the kernel-side eBPF program at
//! `kala-ebpf/` and this user-space loader.
//!
//! `FrameRecord` MUST stay byte-for-byte identical to the
//! `FrameRecord` declared in `kala-ebpf/src/main.rs`. If you change
//! either, change both and rebuild.

use std::ptr;

/// One ring-buffer record emitted by the BPF program on every
/// `Surface::queueBuffer` call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameRecord {
    /// Kernel monotonic time at the moment `queueBuffer` was called,
    /// in nanoseconds (see `bpf_ktime_get_ns`).
    pub ktime_ns: u64,

    /// The buffer pointer argument passed to `queueBuffer`. Used as a
    /// per-back-buffer key to undo triple-buffering interleave.
    pub buffer: u64,
}

impl FrameRecord {
    /// Reinterpret a ring-buffer slice as a [`FrameRecord`].
    ///
    /// Aya's `RingBuf::next()` yields `&[u8]` slices that point at the
    /// kernel-allocated record. Alignment of the underlying mmap is
    /// page-aligned, but the safe move is `read_unaligned` to keep
    /// miri / sanitizers happy even on architectures that would
    /// tolerate the misalignment in practice.
    ///
    /// # Safety
    ///
    /// Caller must ensure `bytes.len() >= size_of::<FrameRecord>()`.
    #[inline]
    pub unsafe fn from_ring_slice(bytes: &[u8]) -> Self {
        unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<FrameRecord>()) }
    }
}

// Toru — kernel-side eBPF program.
//
// Attaches as a uprobe on `android::Surface::queueBuffer` inside
// `/system/lib64/libgui.so`. On every call, captures the current
// monotonic kernel time and the buffer pointer argument, then submits
// a `FrameRecord` to the user-space ring buffer.
//
// User-space side lives in the `toru` crate.

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::bpf_ktime_get_ns,
    macros::{map, uprobe},
    maps::RingBuf,
    programs::ProbeContext,
};

/// Wire format shared with user-space. Must stay byte-for-byte
/// identical to `toru::wire::FrameRecord`.
#[repr(C)]
pub struct FrameRecord {
    pub ktime_ns: u64,
    pub buffer: u64,
}

// 256 KiB ring — power-of-2 multiple of 4K page size, enough to absorb
// a few hundred frames of backlog if user-space stalls.
#[map]
static RING_BUF: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

#[uprobe]
pub fn toru_frame_probe(ctx: ProbeContext) -> u32 {
    match emit(&ctx) {
        Some(()) => 0,
        None => 1,
    }
}

#[inline(always)]
fn emit(ctx: &ProbeContext) -> Option<()> {
    // queueBuffer(ANativeWindowBuffer* buffer, int fence[, output*])
    //   arg(0) = `this` (Surface*)
    //   arg(1) = buffer pointer  <- what we key on
    let buffer: u64 = ctx.arg(1)?;
    let ktime_ns = unsafe { bpf_ktime_get_ns() };

    let mut slot = RING_BUF.reserve::<FrameRecord>(0)?;
    slot.write(FrameRecord { ktime_ns, buffer });
    slot.submit(0);
    Some(())
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // No allocator, no stderr, no `unreachable!`. The verifier will
    // refuse loops that have no upper bound, so use a noreturn intrinsic.
    loop {}
}

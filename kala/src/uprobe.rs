//! Per-PID uprobe handle.
//!
//! Owns one [`aya::Ebpf`] instance and the uprobe program inside it.
//! On drop the program is automatically unloaded.

use aya::{
    Ebpf,
    maps::{MapData, RingBuf},
    programs::UProbe,
};

use crate::error::{ProbeError, Result};
use crate::loader;

const PROGRAM_NAME: &str = "kala_frame_probe";
const LIBGUI_PATH: &str = "/system/lib64/libgui.so";

// Mangled `android::Surface::queueBuffer` family symbols. AOSP has
// refactored the signature several times; we try the most reliable
// entry first and fall back to internal overloads.
//
// 1. `Surface::hook_queueBuffer(ANativeWindow*, ANativeWindowBuffer*, int)`
//    — the C-API trampoline used by every NDK / GL / Vulkan caller.
//      Signature has been stable since the very first Surface
//      implementation, so this is the most reliable probe point on
//      modern devices.
//
// 2. `Surface::queueBufferInternal(ANativeWindow*, ANativeWindowBuffer*, int)`
//    — worker function called from inside hook_queueBuffer. Useful
//      if a vendor inlines (1) away.
//
// 3. Original AOSP <= 10 entry, before the sp<GraphicBuffer> refactor.
//
// 4. Same as (3) with the extra SurfaceQueueBufferOutput* parameter.
//
// 5. Modern overload taking `sp<GraphicBuffer>&&`. Less ideal because
//    arg(1) here is a smart-pointer, not a raw buffer ptr — still
//    works as a uniqueness key for dedup, though.
const QUEUE_BUFFER_SYMBOLS: &[&str] = &[
    // hook_queueBuffer (C trampoline) — best probe point on modern Android.
    "_ZN7android7Surface16hook_queueBufferEP13ANativeWindowP19ANativeWindowBufferi",
    // queueBufferInternal (called by hook_queueBuffer).
    "_ZN7android7Surface19queueBufferInternalEP13ANativeWindowP19ANativeWindowBufferi",
    // Legacy direct entries (AOSP <= 10).
    "_ZN7android7Surface11queueBufferEP19ANativeWindowBufferi",
    "_ZN7android7Surface11queueBufferEP19ANativeWindowBufferiPNS_24SurfaceQueueBufferOutputE",
    // Modern sp<GraphicBuffer> overloads (AOSP 12+).
    "_ZN7android7Surface11queueBufferEONS_2spINS_13GraphicBufferEEEiPNS_24SurfaceQueueBufferOutputE",
    "_ZN7android7Surface11queueBufferERKNS_2spINS_13GraphicBufferEEERKNS1_INS_5FenceEEEPNS_24SurfaceQueueBufferOutputE",
];

pub struct QueueBufferProbe {
    bpf: Ebpf,
    /// Owned ring-buffer reader. Created once at attach: every pump
    /// re-mmaps two pages and re-raises kernel traffic if we rebuilt
    /// it per call, and a fresh reader re-reads the consumer cursor
    /// from the meta page anyway, so one persistent reader is both
    /// cheaper and simpler to reason about.
    ring: RingBuf<MapData>,
}

impl QueueBufferProbe {
    /// Attach a fresh eBPF program to the given PID.
    pub fn attach(pid: i32) -> Result<Self> {
        let mut bpf = loader::load()?;

        {
            let program: &mut UProbe = bpf
                .program_mut(PROGRAM_NAME)
                .ok_or(ProbeError::SymbolNotFound)?
                .try_into()?;
            program.load()?;

            let mut attached = false;
            let mut last_err: Option<ProbeError> = None;
            for symbol in QUEUE_BUFFER_SYMBOLS {
                match program.attach(Some(*symbol), 0, LIBGUI_PATH, Some(pid)) {
                    Ok(_) => {
                        attached = true;
                        break;
                    }
                    Err(e) => last_err = Some(ProbeError::from(e)),
                }
            }
            if !attached {
                return Err(last_err.unwrap_or(ProbeError::SymbolNotFound));
            }
        }

        let ring = RingBuf::try_from(bpf.take_map("RING_BUF").ok_or(ProbeError::SymbolNotFound)?)?;

        Ok(Self { bpf, ring })
    }

    /// The persistent ring-buffer reader for polling/reading.
    pub fn ring(&mut self) -> &mut RingBuf<MapData> {
        &mut self.ring
    }
}

impl Drop for QueueBufferProbe {
    fn drop(&mut self) {
        if let Some(program) = self
            .bpf
            .program_mut(PROGRAM_NAME)
            .and_then(|p| TryInto::<&mut UProbe>::try_into(p).ok())
        {
            let _ = program.unload();
        }
    }
}

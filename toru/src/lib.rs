//! # toru
//!
//! Per-app frame-time probe for Android, powered by eBPF uprobes on
//! libgui's `Surface::queueBuffer`. Drop it into any tool that needs
//! real, per-frame timing for a target app — no copying BPF programs
//! or ring-buffer plumbing required.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::time::Duration;
//! use toru::FrameProbe;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut probe = FrameProbe::new()?;
//! probe.attach(1234)?; // PID of the target app
//!
//! while let Some((pid, frametime)) =
//!     probe.recv_with_deadline(Duration::from_millis(50))
//! {
//!     println!("pid {pid}: {:.2} ms", frametime.as_secs_f64() * 1e3);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Requirements
//!
//! * Linux kernel with eBPF uprobe and ring-buffer support
//!   (kernel 5.8+; tested on 5.10).
//! * Root / `CAP_SYS_ADMIN` + `CAP_BPF` at runtime.
//! * `/system/lib64/libgui.so` present (i.e. a real Android image).
//! * Build time: `bpf-linker` available in `PATH` (`cargo install
//!   bpf-linker`). The pre-built BPF object is embedded into the
//!   library, so downstream consumers do **not** need bpf-linker
//!   when they depend on `toru` from crates.io.
//!
//! ## How it works
//!
//! `Surface::queueBuffer` is called by every Android frame producer
//! (NDK, OpenGL, Vulkan, system UI) right before a frame is handed to
//! SurfaceFlinger. We attach a uprobe to it, capture the buffer
//! pointer (used as a per-back-buffer key) and the kernel monotonic
//! timestamp, and submit a [`FrameRecord`](wire::FrameRecord) to a
//! per-PID ring buffer.
//!
//! User-space polls each ring buffer with `mio`, groups records by
//! buffer pointer to undo triple-buffering, and yields one frametime
//! per displayed frame.

mod error;
mod loader;
mod tracker;
mod uprobe;

pub mod wire;

pub use error::{ProbeError, Result};

use std::{
    collections::{HashMap, VecDeque},
    os::unix::io::AsRawFd,
    time::Duration,
};

use mio::{Events, Interest, Poll, Token, event::Event, unix::SourceFd};

use tracker::FrameTracker;
use uprobe::QueueBufferProbe;

/// Linux process id.
pub type Pid = i32;

const EVENT_CAPACITY: usize = 1024;

/// Owns one or more per-PID uprobes and exposes a single polling
/// interface over their ring buffers.
///
/// Per-PID cleanup is automatic: dropping the [`FrameProbe`] drops
/// every attached uprobe, which unloads its eBPF program.
pub struct FrameProbe {
    poll: Option<Poll>,
    targets: HashMap<Pid, FrameTracker>,
    pending: VecDeque<Pid>,
}

impl FrameProbe {
    /// Create a probe with no PIDs attached yet.
    pub fn new() -> Result<Self> {
        Ok(Self {
            poll: None,
            targets: HashMap::new(),
            pending: VecDeque::with_capacity(EVENT_CAPACITY),
        })
    }

    /// Start watching `pid`. Idempotent — re-attaching the same PID is
    /// a no-op so callers don't have to track state.
    pub fn attach(&mut self, pid: Pid) -> Result<()> {
        if self.targets.contains_key(&pid) {
            return Ok(());
        }
        let probe = QueueBufferProbe::attach(pid)?;
        self.targets.insert(pid, FrameTracker::new(probe));
        self.rebuild_poll()
    }

    /// Stop watching `pid`. Returns `true` if the PID was attached.
    pub fn detach(&mut self, pid: Pid) -> Result<bool> {
        let removed = self.targets.remove(&pid).is_some();
        if removed {
            self.rebuild_poll()?;
        }
        Ok(removed)
    }

    /// Block for up to `timeout`, returning the first `(pid, frametime)`
    /// that arrives. Returns `None` if nothing arrived or there is
    /// nothing attached yet.
    pub fn recv_with_deadline(&mut self, timeout: Duration) -> Option<(Pid, Duration)> {
        if self.pending.is_empty() {
            if let Some(poll) = self.poll.as_mut() {
                let mut events = Events::with_capacity(EVENT_CAPACITY);
                let _ = poll.poll(&mut events, Some(timeout));
                self.pending.extend(events.iter().map(pid_from_event));
            }
            // Rebuild the poll registry to drop any token whose
            // backing fd has gone away. Errors here aren't fatal —
            // the next `recv_with_deadline` will try again.
            let _ = self.rebuild_poll();
        }

        loop {
            let pid = self.pending.pop_front()?;
            let tracker = self.targets.get_mut(&pid)?;
            match tracker.pump() {
                Ok(Some(duration)) => return Some((pid, duration)),
                Ok(None) => continue,
                Err(_) => continue,
            }
        }
    }

    /// Number of currently attached PIDs.
    pub fn attached(&self) -> usize {
        self.targets.len()
    }

    fn rebuild_poll(&mut self) -> Result<()> {
        let poll = Poll::new()?;
        for (pid, tracker) in &mut self.targets {
            let fd = tracker.probe.ring_buf()?.as_raw_fd();
            poll.registry().register(
                &mut SourceFd(&fd),
                Token(*pid as usize),
                Interest::READABLE,
            )?;
        }
        self.poll = Some(poll);
        Ok(())
    }
}

fn pid_from_event(event: &Event) -> Pid {
    let Token(raw) = event.token();
    raw as Pid
}

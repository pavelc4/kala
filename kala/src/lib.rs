//! # kala-ebpf
//!
//! Per-app frame-time probe for Android, powered by eBPF uprobes on
//! libgui's `Surface::queueBuffer`. Drop it into any tool that needs
//! real, per-frame timing for a target app — no copying BPF programs
//! or ring-buffer plumbing required.
//!
//! Repo: <https://github.com/pavelc4/kala>.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::time::Duration;
//! use kala_ebpf::FrameProbe;
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
//! * Build time (maintainers only): regenerating the shipped eBPF
//!   asset needs `bpf-linker` (`cargo install bpf-linker`) and the
//!   pinned nightly — `KALA_REBUILD_BPF=1 cargo build`. The pre-built
//!   BPF object is embedded into the library, so downstream consumers
//!   do **not** need bpf-linker when they depend on `kala-ebpf` from
//!   crates.io.
//!
//! ## How it works
//!
//! `Surface::queueBuffer` is called by every Android frame producer
//! (NDK, OpenGL, Vulkan, system UI) right before a frame is handed to
//! `SurfaceFlinger`. We attach a uprobe to it, capture the buffer
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
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when the memlock limit cannot be raised
    /// or the embedded eBPF program fails to initialize.
    pub fn new() -> Result<Self> {
        Ok(Self {
            poll: None,
            targets: HashMap::new(),
            pending: VecDeque::with_capacity(EVENT_CAPACITY),
        })
    }

    /// Start watching `pid`. Idempotent — re-attaching the same PID is
    /// a no-op so callers don't have to track state.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when the symbol cannot be resolved in
    /// `libgui.so` or attaching (poll registry rebuild) fails.
    pub fn attach(&mut self, pid: Pid) -> Result<()> {
        if pid <= 0 {
            return Err(ProbeError::InvalidPid(pid));
        }
        if self.targets.contains_key(&pid) {
            return Ok(());
        }
        let probe = QueueBufferProbe::attach(pid)?;
        self.targets.insert(pid, FrameTracker::new(probe));
        self.rebuild_poll()
    }

    /// Stop watching `pid`. Returns `true` if the PID was attached.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when rebuilding the poll registry fails
    /// after removal; the PID is removed either way.
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
        // Nothing queued? Poll once, then rebuild the registry so any
        // token whose backing fd went away is dropped. (When `poll` is
        // `None` nothing is attached at all, so rebuilding is a no-op.)
        // Errors aren't fatal — the next call retries.
        if self.pending.is_empty()
            && let Some(poll) = self.poll.as_mut()
        {
            let mut events = Events::with_capacity(EVENT_CAPACITY);
            let _ = poll.poll(&mut events, Some(timeout));
            self.pending
                .extend(events.iter().filter_map(pid_from_event));
            let _ = self.rebuild_poll();
        }

        loop {
            let pid = self.pending.pop_front()?;
            let tracker = self.targets.get_mut(&pid)?;
            if let Ok(Some(duration)) = tracker.pump() {
                return Some((pid, duration));
            }
            // Either no frame was queued for this PID or the ring
            // errored — both mean "try the next pending PID".
        }
    }

    /// Number of currently attached PIDs.
    #[must_use]
    pub fn attached(&self) -> usize {
        self.targets.len()
    }

    fn rebuild_poll(&mut self) -> Result<()> {
        let poll = Poll::new()?;
        for (pid, tracker) in &mut self.targets {
            let fd = tracker.probe.ring_buf()?.as_raw_fd();
            // `attach` guarantees a positive pid, and a positive i32
            // always fits usize — this conversion cannot fail.
            let token = Token(usize::try_from(*pid).expect("pid validated positive in attach"));
            poll.registry().register(
                &mut SourceFd(&fd),
                token,
                Interest::READABLE,
            )?;
        }
        self.poll = Some(poll);
        Ok(())
    }
}

/// Decode the `Pid` inside a mio token. Tokens were built from
/// validated PIDs, so anything that fails to narrow is the kernel
/// handing back a token we never registered — drop it instead of
/// inventing a frame for a phantom PID.
fn pid_from_event(event: &Event) -> Option<Pid> {
    Pid::try_from(event.token().0).ok()
}

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
    time::{Duration, Instant},
};

use mio::{Events, Interest, Poll, Token, event::Event, unix::SourceFd};

use tracker::FrameTracker;
use uprobe::QueueBufferProbe;

/// Linux process id.
pub type Pid = i32;

/// Telemetry event returned by [`FrameProbe::recv_with_stats`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameEvent {
    /// Linux process id that emitted the frame.
    pub pid: Pid,
    /// Measured frametime delta.
    pub frametime: Duration,
    /// Execution time of the `pump()` call in nanoseconds.
    pub pump_latency_ns: u64,
    /// Ring records consumed to produce this frame.
    pub ring_records: usize,
    /// Sequence count of frames drained in this epoll burst (1-based).
    pub burst_count: usize,
}

const EVENT_CAPACITY: usize = 1024;

/// Owns one or more per-PID uprobes and exposes a single polling
/// interface over their ring buffers.
///
/// Per-PID cleanup is automatic: dropping the [`FrameProbe`] drops
/// every attached uprobe, which unloads its eBPF program.
pub struct FrameProbe {
    poll: Poll,
    events: Events,
    targets: HashMap<Pid, FrameTracker>,
    pending: VecDeque<Pid>,
    burst_count: usize,
}

impl FrameProbe {
    /// Create a probe with no PIDs attached yet.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when the poller cannot be created, the
    /// memlock limit cannot be raised, or the embedded eBPF program
    /// fails to initialize.
    pub fn new() -> Result<Self> {
        Ok(Self {
            poll: Poll::new()?,
            events: Events::with_capacity(EVENT_CAPACITY),
            targets: HashMap::new(),
            pending: VecDeque::with_capacity(EVENT_CAPACITY),
            burst_count: 0,
        })
    }

    /// Start watching `pid`. Idempotent — re-attaching the same PID is
    /// a no-op so callers don't have to track state.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when the symbol cannot be resolved in
    /// `libgui.so` or the ring buffer cannot be registered with the
    /// poller (in which case nothing is attached).
    ///
    /// # Panics
    ///
    /// Panics only if `pid` were negative when building the poll
    /// token — unreachable, because the `pid <= 0` guard above runs
    /// first in this same function.
    pub fn attach(&mut self, pid: Pid) -> Result<()> {
        if pid <= 0 {
            return Err(ProbeError::InvalidPid(pid));
        }
        if self.targets.contains_key(&pid) {
            return Ok(());
        }
        let mut tracker = FrameTracker::new(QueueBufferProbe::attach(pid)?);
        let fd = tracker.probe.ring().as_raw_fd();
        // `pid` was validated positive above; this cannot fail.
        let token = Token(usize::try_from(pid).expect("pid validated positive in attach"));
        self.poll
            .registry()
            .register(&mut SourceFd(&fd), token, Interest::READABLE)?;
        self.targets.insert(pid, tracker);
        Ok(())
    }

    /// Stop watching `pid`. Returns `true` if the PID was attached.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when deregistering the ring buffer fails;
    /// the PID is removed either way.
    pub fn detach(&mut self, pid: Pid) -> Result<bool> {
        // Remove first: the tracker keeps its map (and fd) alive, so
        // the epoll CTL below still refers to a living descriptor, and
        // a failed deregistration still leaves the PID detached.
        let Some(mut tracker) = self.targets.remove(&pid) else {
            return Ok(false);
        };
        let fd = tracker.probe.ring().as_raw_fd();
        self.poll.registry().deregister(&mut SourceFd(&fd))?;
        Ok(true)
    }

    /// Drains the pending queue, polling the ring buffers only when it
    /// runs dry. Returns `None` if nothing arrived or there is nothing
    /// attached yet.
    pub fn recv_with_deadline(&mut self, timeout: Duration) -> Option<(Pid, Duration)> {
        loop {
            if let Some(pid) = self.pending.pop_front() {
                if let Some(tracker) = self.targets.get_mut(&pid) {
                    if let Some(duration) = tracker.pump() {
                        // Re-queue so remaining records in this ring buffer get pumped
                        // on subsequent calls without waiting on edge-triggered epoll.
                        self.pending.push_back(pid);
                        return Some((pid, duration));
                    }
                }
                // When tracker.pump() returns None, this PID's ring buffer is completely drained.
                continue;
            }

            self.events.clear();
            let _ = self.poll.poll(&mut self.events, Some(timeout));
            if self.events.is_empty() {
                return None;
            }
            self.pending
                .extend(self.events.iter().filter_map(pid_from_event));
        }
    }

    /// Drains the pending queue, polling the ring buffers only when it
    /// runs dry. Returns detailed telemetry per frame including execution
    /// latency of the `pump()` call and burst count since last epoll wait.
    pub fn recv_with_stats(&mut self, timeout: Duration) -> Option<FrameEvent> {
        loop {
            if let Some(pid) = self.pending.pop_front() {
                if let Some(tracker) = self.targets.get_mut(&pid) {
                    let t0 = Instant::now();
                    let (duration, records) = tracker.pump_counted();
                    let pump_latency_ns =
                        u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    if let Some(duration) = duration {
                        self.burst_count += 1;
                        self.pending.push_back(pid);
                        return Some(FrameEvent {
                            pid,
                            frametime: duration,
                            pump_latency_ns,
                            ring_records: records,
                            burst_count: self.burst_count,
                        });
                    }
                }
                // When tracker.pump_counted() returns None, this PID's ring buffer is completely drained.
                continue;
            }

            self.burst_count = 0;
            self.events.clear();
            let _ = self.poll.poll(&mut self.events, Some(timeout));
            if self.events.is_empty() {
                return None;
            }
            self.pending
                .extend(self.events.iter().filter_map(pid_from_event));
        }
    }

    /// Number of currently attached PIDs.
    #[must_use]
    pub fn attached(&self) -> usize {
        self.targets.len()
    }

    /// Drain one queued record from the first attached tracker without
    /// waiting for epoll. Bench/tests use this to attribute costs to
    /// the ring-read path alone.
    pub fn pump_now(&mut self) -> Option<Duration> {
        self.targets
            .values_mut()
            .next()
            .and_then(FrameTracker::pump)
    }
}

/// Decode the `Pid` inside a mio token. Tokens were built from
/// validated PIDs, so anything that fails to narrow is the kernel
/// handing back a token we never registered — drop it instead of
/// inventing a frame for a phantom PID.
fn pid_from_event(event: &Event) -> Option<Pid> {
    Pid::try_from(event.token().0).ok()
}

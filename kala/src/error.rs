//! Error types surfaced by the public API.
//!
//! Internal aya / mio failures are wrapped via `From` so call sites
//! only deal with [`ProbeError`].

use std::io;

use aya::{EbpfError, maps::MapError, programs::ProgramError};
use thiserror::Error;

use crate::Pid;

/// Convenience alias used throughout the crate's public API.
pub type Result<T> = std::result::Result<T, ProbeError>;

/// Errors returned by [`FrameProbe`](crate::FrameProbe) operations.
#[derive(Debug, Error)]
pub enum ProbeError {
    /// The embedded eBPF object failed to load. Most commonly: missing
    /// `CAP_BPF`, an old kernel without ring-buffer support, or a
    /// memlock rlimit that couldn't be raised.
    #[error("failed to load embedded eBPF object: {0}")]
    Load(#[from] EbpfError),

    /// An eBPF program operation (load / attach / unload) failed.
    #[error("eBPF program operation failed: {0}")]
    Program(#[from] ProgramError),

    /// An eBPF map operation (open / read) failed.
    #[error("eBPF map operation failed: {0}")]
    Map(#[from] MapError),

    /// Generic system I/O failure (poll setup, fd handling, etc.).
    #[error("system I/O failure: {0}")]
    Io(#[from] io::Error),

    /// None of the known mangled `Surface::queueBuffer` symbols were
    /// resolvable inside `/system/lib64/libgui.so` for this PID.
    #[error("Surface::queueBuffer symbol not found in libgui.so")]
    SymbolNotFound,

    /// The caller passed a non-positive PID. Linux PIDs start at 1;
    /// anything else can never be watched.
    #[error("invalid pid: expected a positive Linux pid, got {0}")]
    InvalidPid(Pid),
}

//! On-device benchmark: allocation, CPU, and memory cost of a probe run.
//!
//! ```text
//! cargo build --release --example bench   # same android build as basic
//! adb push target/aarch64-linux-android/release/examples/bench /data/local/tmp/
//! adb shell su -c "/data/local/tmp/bench <pid> [seconds]"   # default 10
//! ```

use std::{
    alloc::{GlobalAlloc, Layout, System},
    env, process,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use kala_ebpf::FrameProbe;

static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
/// Size histogram (bucket powers of two: <16, <64, <256, <1Ki, <4Ki, ≥4Ki)
/// to attribute where the per-frame allocations happen.
static ALLOC_BUCKETS: [AtomicU64; 6] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn bucket_of(size: usize) -> usize {
    match size {
        0..=15 => 0,
        16..=63 => 1,
        64..=255 => 2,
        256..=1_023 => 3,
        1_024..=4_095 => 4,
        _ => 5,
    }
}

/// Counts allocations delegated to the system allocator. Reallocs go
/// through the default `alloc + copy + dealloc` path, so both sides
/// count; steady-state numbers are still meaningful.
struct Counting;

// SAFETY: forwards every call to `System`, which is already thread-safe.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        ALLOC_BUCKETS[bucket_of(layout.size())].fetch_add(1, Ordering::Relaxed);
        // SAFETY: callers of `alloc` obey the `GlobalAlloc` contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with the same layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// User + system CPU time of this process so far, in microseconds.
fn cpu_us() -> u64 {
    // SAFETY: `usage` is fully written by getrusage before we read it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let got = unsafe { libc::getrusage(libc::RUSAGE_SELF, std::ptr::from_mut(&mut usage)) };
    if got != 0 {
        return 0;
    }
    let (u, s) = (&usage.ru_utime, &usage.ru_stime);
    let secs = u64::try_from(u.tv_sec + s.tv_sec).unwrap_or(0);
    let usecs = u64::try_from(u.tv_usec + s.tv_usec).unwrap_or(0);
    secs * 1_000_000 + usecs
}

/// (current, peak) resident set of this process, in kB.
fn rss_kb() -> (u64, u64) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let read = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key)?.trim().split(' ').next()?.parse().ok())
    };
    (read("VmRSS:").unwrap_or(0), read("VmHWM:").unwrap_or(0))
}

/// u64→f64 that saturates instead of lying about tiny averages.
fn f(v: u64) -> f64 {
    f64::from(u32::try_from(v).unwrap_or(u32::MAX))
}

/// Busy-drain `pump()` for `secs` without touching epoll. Every
/// allocation counted inside this window belongs to the
/// ring→record read path; the polling loop numbers that follow can
/// then be read as "loop − pump".
fn calibrate(probe: &mut FrameProbe, secs: u64) {
    println!("calibration: busy-pumping pump() for {secs}s...");
    let (c0, b0, _) = (
        ALLOC_CALLS.load(Ordering::Relaxed),
        ALLOC_BYTES.load(Ordering::Relaxed),
        cpu_us(),
    );
    let mut pump_calls: u32 = 0;
    let mut pump_hits: u32 = 0;
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        pump_calls += 1;
        if probe.pump_now().is_some() {
            pump_hits += 1;
        }
    }
    let (c1, b1, _) = (
        ALLOC_CALLS.load(Ordering::Relaxed),
        ALLOC_BYTES.load(Ordering::Relaxed),
        cpu_us(),
    );
    println!(
        "calib:     {pump_calls} pump calls, {pump_hits} drained records, {} allocs, {} B  ({:.0} B/call)",
        c1 - c0,
        b1 - b0,
        f(b1 - b0) / f((c1 - c0).max(1)),
    );
}

fn main() {
    let Some(pid_arg) = env::args().nth(1) else {
        eprintln!("usage: bench <pid> [seconds]");
        process::exit(2);
    };
    let Ok(pid) = pid_arg.parse() else {
        eprintln!("pid must be numeric: {pid_arg}");
        process::exit(2);
    };
    let run_secs: u64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let mut probe = match FrameProbe::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FrameProbe::new failed: {e}");
            process::exit(1);
        }
    };
    if let Err(e) = probe.attach(pid) {
        eprintln!("attach({pid}) failed: {e}");
        process::exit(1);
    }

    let calib_secs = 5;
    calibrate(&mut probe, calib_secs);

    // Snapshot after setup AND calibration, so the numbers below are
    // the steady-state polling loop only — no BPF-load cost, no
    // calibration-phase allocations folded in.
    let snap = || {
        (
            ALLOC_CALLS.load(Ordering::Relaxed),
            ALLOC_BYTES.load(Ordering::Relaxed),
            cpu_us(),
        )
    };
    let (calls0, bytes0, cpu0) = snap();
    let buckets0: Vec<u64> = ALLOC_BUCKETS
        .iter()
        .map(|b| b.load(Ordering::Relaxed))
        .collect();
    let rss_start = rss_kb().0;
    let wall = Instant::now();

    let deadline = wall + Duration::from_secs(run_secs);
    let mut frame_times: Vec<Duration> = Vec::new();
    while Instant::now() < deadline {
        if let Some((_, ft)) = probe.recv_with_deadline(Duration::from_millis(50)) {
            frame_times.push(ft);
        }
    }

    let (calls1, bytes1, cpu1) = snap();
    let buckets: Vec<u64> = ALLOC_BUCKETS
        .iter()
        .map(|b| b.load(Ordering::Relaxed))
        .collect();
    let (rss_end, rss_peak) = rss_kb();

    let frames = u64::try_from(frame_times.len()).unwrap_or(1).max(1);
    let calls = calls1 - calls0;
    let bytes = bytes1 - bytes0;
    let cpu = cpu1.saturating_sub(cpu0);
    let wall_us = u64::try_from(wall.elapsed().as_micros()).unwrap_or(1).max(1);

    // All observed values are far below 2^52; saturate instead of lying.
    println!("frames:    {} in {run_secs}s", frame_times.len());
    if let Some(median) = frame_times.get(frame_times.len() / 2).copied() {
        let ms = median.as_secs_f64() * 1e3;
        println!("frametime: median {ms:.2} ms  ({:.1} fps)", 1e3 / ms);
    }
    println!(
        "cpu:       {cpu} µs  ({:.2}% of wall, {:.1} µs/frame)",
        f(cpu) / f(wall_us) * 100.0,
        f(cpu) / f(frames),
    );
    println!(
        "allocs:    {calls} calls, {bytes} bytes  ({:.1}/frame, {:.0} B/frame)",
        f(calls) / f(frames),
        f(bytes) / f(frames),
    );
    let deltas: Vec<String> = buckets
        .iter()
        .zip(&buckets0)
        .map(|(a, b)| (a - b).to_string())
        .collect();
    println!(
        "buckets:   [{}] (<16 <64 <256 <1K <4K ≥4K)",
        deltas.join(" ")
    );
    println!(
        "rss:       start {rss_start} kB, end {rss_end} kB, peak {rss_peak} kB"
    );
}

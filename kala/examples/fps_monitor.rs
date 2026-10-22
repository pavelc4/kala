//! Real-time FPS Monitor with 1% Low & 0.1% Low analytics.
//!
//! Measures real-time gaming FPS, frame-time pacing, 1% low, and 0.1% low metrics
//! over a sliding window without impacting game performance.
//!
//! Usage:
//! ```text
//! cargo build --release --example fps_monitor
//! adb push target/aarch64-linux-android/release/examples/fps_monitor /data/local/tmp/
//! adb shell su -c "/data/local/tmp/fps_monitor <pid> [duration_seconds]"
//! ```

use std::{
    env, process,
    time::{Duration, Instant},
};

use kala_ebpf::FrameProbe;

fn compute_percentile_low(sorted_frametimes: &[Duration], percentile: f64) -> f64 {
    if sorted_frametimes.is_empty() {
        return 0.0;
    }
    // For 1% Low: look at the slowest 1% of frames (highest frametimes)
    let idx = ((sorted_frametimes.len() as f64) * (1.0 - percentile / 100.0)).floor() as usize;
    let clamped_idx = idx.min(sorted_frametimes.len() - 1);
    let dt_secs = sorted_frametimes[clamped_idx].as_secs_f64();
    if dt_secs > 0.0 { 1.0 / dt_secs } else { 0.0 }
}

fn main() {
    let Some(pid_arg) = env::args().nth(1) else {
        eprintln!("Usage: fps_monitor <pid> [duration_seconds]");
        eprintln!("Example: fps_monitor $(pidof com.mobile.legends:UnityKillsMe) 30");
        process::exit(2);
    };

    let Ok(pid) = pid_arg.parse() else {
        eprintln!("Error: PID must be numeric: {pid_arg}");
        process::exit(2);
    };

    let run_secs: u64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let mut probe = match FrameProbe::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to initialize eBPF probe: {e}");
            process::exit(1);
        }
    };

    if let Err(e) = probe.attach(pid) {
        eprintln!("Failed to attach probe to PID {pid}: {e}");
        process::exit(1);
    }

    println!("pid: {pid} ({run_secs}s)");
    println!(" sec    fps  1%low     min     max  frames");

    let start_time = Instant::now();
    let total_deadline = start_time + Duration::from_secs(run_secs);

    let mut all_frametimes: Vec<Duration> = Vec::with_capacity(120 * run_secs as usize);
    let mut window_frametimes: Vec<Duration> = Vec::with_capacity(240);
    let mut last_window_report = Instant::now();
    let window_duration = Duration::from_millis(1000);

    while Instant::now() < total_deadline {
        if let Some((_target_pid, ft)) = probe.recv_with_deadline(Duration::from_millis(50)) {
            all_frametimes.push(ft);
            window_frametimes.push(ft);
        }

        if last_window_report.elapsed() >= window_duration {
            let elapsed_total = start_time.elapsed().as_secs();
            let count = window_frametimes.len();
            let avg_fps = if !window_frametimes.is_empty() {
                let sum_secs: f64 = window_frametimes.iter().map(Duration::as_secs_f64).sum();
                count as f64 / sum_secs
            } else {
                0.0
            };

            let min_ms = window_frametimes
                .iter()
                .min()
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            let max_ms = window_frametimes
                .iter()
                .max()
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);

            let mut sorted = window_frametimes.clone();
            sorted.sort();
            let low_1pct = compute_percentile_low(&sorted, 1.0);

            println!(
                "{:>4}  {:>5.1}  {:>5.1}  {:>5.1}ms {:>5.1}ms  {:>6}",
                elapsed_total, avg_fps, low_1pct, min_ms, max_ms, count
            );

            window_frametimes.clear();
            last_window_report = Instant::now();
        }
    }

    if all_frametimes.is_empty() {
        println!("\nno frames captured (is the app rendering?)");
        return;
    }

    all_frametimes.sort();
    let total_frames = all_frametimes.len();
    let sum_secs: f64 = all_frametimes.iter().map(Duration::as_secs_f64).sum();
    let global_avg_fps = total_frames as f64 / sum_secs;
    let median_ms = all_frametimes[total_frames / 2].as_secs_f64() * 1000.0;
    let p99_ms =
        all_frametimes[((total_frames as f64) * 0.99).floor() as usize].as_secs_f64() * 1000.0;
    let low_1pct = compute_percentile_low(&all_frametimes, 1.0);
    let low_01pct = compute_percentile_low(&all_frametimes, 0.1);

    println!(
        "\nsummary: {total_frames} frames, {global_avg_fps:.1} fps (median {median_ms:.2}ms, p99 {p99_ms:.2}ms, 1%low {low_1pct:.1} fps, 0.1%low {low_01pct:.1} fps)"
    );
}

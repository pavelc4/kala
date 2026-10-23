//! Multi-target frame probe example.
//!
//! Demonstrates attaching to multiple processes concurrently and streaming
//! frametimes fairly using Kala's round-robin pending queue.
//!
//! Usage:
//! ```text
//! cargo build --release --example multi_target
//! adb push target/aarch64-linux-android/release/examples/multi_target /data/local/tmp/
//! adb shell su -c "/data/local/tmp/multi_target <pid1> <pid2> [...]"
//! ```

#![allow(clippy::pedantic)]

use std::{
    collections::HashMap,
    env, process,
    time::{Duration, Instant},
};

use kala_ebpf::FrameProbe;

struct TargetStats {
    frames: u64,
    total_time: Duration,
    last_frametime: Duration,
}

impl Default for TargetStats {
    fn default() -> Self {
        Self {
            frames: 0,
            total_time: Duration::ZERO,
            last_frametime: Duration::ZERO,
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: multi_target <pid1> [pid2] [pid3] ...");
        eprintln!(
            "Example: multi_target $(pidof com.mobile.legends:UnityKillsMe) $(pidof bin.mt.plus)"
        );
        process::exit(2);
    }

    let mut pids: Vec<i32> = Vec::new();
    for arg in &args {
        match arg.parse::<i32>() {
            Ok(p) if p > 0 => pids.push(p),
            _ => eprintln!("Warning: skipping invalid PID '{arg}'"),
        }
    }

    if pids.is_empty() {
        eprintln!("Error: No valid PIDs provided.");
        process::exit(1);
    }

    let mut probe = match FrameProbe::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to initialize eBPF probe: {e}");
            process::exit(1);
        }
    };

    for &pid in &pids {
        match probe.attach(pid) {
            Ok(()) => println!("Successfully attached probe to PID {pid}"),
            Err(e) => eprintln!("Failed to attach probe to PID {pid}: {e}"),
        }
    }

    if probe.attached() == 0 {
        eprintln!("Error: None of the target PIDs could be attached.");
        process::exit(1);
    }

    println!("watching: {:?}\n", pids);
    println!("  time      pid  frames     last      fps");

    let mut stats: HashMap<i32, TargetStats> = HashMap::new();
    let mut last_report = Instant::now();
    let start_time = Instant::now();
    let report_interval = Duration::from_secs(1);

    loop {
        // Poll with a 50ms deadline
        if let Some((pid, ft)) = probe.recv_with_deadline(Duration::from_millis(50)) {
            let entry = stats.entry(pid).or_default();
            entry.frames += 1;
            entry.total_time += ft;
            entry.last_frametime = ft;
        }

        // Print stats every 1 second
        if last_report.elapsed() >= report_interval {
            let elapsed = start_time.elapsed().as_secs_f64();
            for (&pid, stat) in &stats {
                let ms = stat.last_frametime.as_secs_f64() * 1000.0;
                let fps = if stat.total_time.as_secs_f64() > 0.0 {
                    stat.frames as f64 / stat.total_time.as_secs_f64()
                } else {
                    0.0
                };
                println!(
                    "{:>5.1}s  {:>7}  {:>6}  {:>5.2}ms  {:>7.1}",
                    elapsed, pid, stat.frames, ms, fps
                );
            }
            last_report = Instant::now();
        }
    }
}

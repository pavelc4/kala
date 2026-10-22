//! Real-Time Jank & Stutter Detector.
//!
//! Analyzes frame delivery to catch micro-stutters, frame pacing spikes,
//! and dropped VSYNC deadlines in real-time.
//!
//! Usage:
//! ```text
//! cargo build --release --example jank_detector
//! adb push target/aarch64-linux-android/release/examples/jank_detector /data/local/tmp/
//! adb shell su -c "/data/local/tmp/jank_detector <pid> [target_fps]"
//! ```

use std::{
    env, process,
    time::{Duration, Instant},
};

use kala_ebpf::FrameProbe;

fn main() {
    let Some(pid_arg) = env::args().nth(1) else {
        eprintln!("Usage: jank_detector <pid> [target_fps]");
        eprintln!("Example: jank_detector $(pidof com.mobile.legends:UnityKillsMe) 120");
        process::exit(2);
    };

    let Ok(pid) = pid_arg.parse() else {
        eprintln!("Error: PID must be numeric: {pid_arg}");
        process::exit(2);
    };

    let target_fps: f64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60.0);

    let expected_ft_ms = 1000.0 / target_fps;
    // Janks:
    // Mild: > 1.5x expected frame time (1 missed VSYNC frame)
    // Moderate: > 2.5x expected frame time (2+ missed frames)
    // Severe / Stall: > 5.0x expected frame time (> 80ms at 60Hz)
    let mild_threshold_ms = expected_ft_ms * 1.5;
    let moderate_threshold_ms = expected_ft_ms * 2.5;
    let severe_threshold_ms = expected_ft_ms * 5.0;

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

    println!("pid: {pid}, target: {target_fps:.0} fps ({expected_ft_ms:.2}ms)");
    println!(
        "thresholds: mild >{mild_threshold_ms:.1}ms, moderate >{moderate_threshold_ms:.1}ms, severe >{severe_threshold_ms:.1}ms\n"
    );
    println!("    time   frametime  dropped  level");

    let mut total_frames: u64 = 0;
    let mut mild_janks: u64 = 0;
    let mut moderate_janks: u64 = 0;
    let mut severe_stalls: u64 = 0;
    let start_time = Instant::now();

    loop {
        if let Some((_pid, ft)) = probe.recv_with_deadline(Duration::from_millis(100)) {
            total_frames += 1;
            let ft_ms = ft.as_secs_f64() * 1000.0;
            let elapsed = start_time.elapsed().as_secs_f64();

            if ft_ms >= severe_threshold_ms {
                severe_stalls += 1;
                let dropped = (ft_ms / expected_ft_ms).floor() as u64;
                println!(
                    "{:>7.1}s   {:>6.2} ms  {:>7}  severe",
                    elapsed, ft_ms, dropped
                );
            } else if ft_ms >= moderate_threshold_ms {
                moderate_janks += 1;
                let dropped = (ft_ms / expected_ft_ms).floor() as u64;
                println!(
                    "{:>7.1}s   {:>6.2} ms  {:>7}  moderate",
                    elapsed, ft_ms, dropped
                );
            } else if ft_ms >= mild_threshold_ms {
                mild_janks += 1;
                println!("{:>7.1}s   {:>6.2} ms  {:>7}  mild", elapsed, ft_ms, 1);
            }
        }

        // Summary every 1200 frames
        if total_frames > 0 && total_frames % 1200 == 0 {
            let total_janks = mild_janks + moderate_janks + severe_stalls;
            let jank_rate = (total_janks as f64 / total_frames as f64) * 100.0;
            println!("checkpoint: {total_frames} frames, {total_janks} janks ({jank_rate:.2}%)\n");
        }
    }
}

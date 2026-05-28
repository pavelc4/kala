//! Minimal demo: attach to a PID, print frametimes for ~10 seconds.
//!
//! ```text
//! cargo build --release --example basic
//! adb push target/aarch64-linux-android/release/examples/basic /data/local/tmp/
//! adb shell su -c "/data/local/tmp/basic <pid>"
//! ```

use std::{env, process, time::{Duration, Instant}};

use toru::FrameProbe;

fn main() {
    let pid: i32 = env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("usage: basic <pid>");
            process::exit(2);
        });

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
    eprintln!("attached to pid {pid}; collecting for 10s...");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut count: u64 = 0;
    let mut total = Duration::ZERO;

    while Instant::now() < deadline {
        if let Some((_pid, ft)) = probe.recv_with_deadline(Duration::from_millis(50)) {
            println!("frame {count:>5}: {:.2} ms", ft.as_secs_f64() * 1e3);
            count += 1;
            total += ft;
        }
    }

    if count > 0 {
        let avg = total / count as u32;
        eprintln!(
            "summary: {count} frames, avg {:.2} ms ({:.1} fps)",
            avg.as_secs_f64() * 1e3,
            1.0 / avg.as_secs_f64()
        );
    } else {
        eprintln!("no frames received (is the app rendering?)");
    }
}

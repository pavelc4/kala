//! Unit tests for the per-buffer dedup heuristic in `tracker/mod.rs`.
//!
//! These exercise `BufferTimes::record` directly without loading a
//! BPF program, so they run on any host machine (`cargo test`).

use super::*;
use crate::wire::FrameRecord;
use std::time::Duration;

// One buffer surface: every sighting after the first is a clean
// per-frame delta.
#[test]
fn single_buffer_yields_deltas_after_first_sight() {
    let mut t = BufferTimes::default();
    let frame_ns: u64 = 16_666_667; // 60 Hz
    assert!(t
        .record(FrameRecord { ktime_ns: 1_000, buffer: 7 })
        .is_none());
    for i in 1u64..4 {
        let ft = t
            .record(FrameRecord { ktime_ns: 1_000 + frame_ns * i, buffer: 7 })
            .expect("steady single-buffer deltas must surface");
        assert_eq!(ft, Duration::from_nanos(frame_ns));
    }
}

// Uneven interleave: buffer A is seen every 2nd tick, B every 3rd,
// C on leftovers — so A's history is *uniquely* the longest from its
// second sighting. The heuristic must then surface exactly the
// per-buffer interval (2×frame) on every later A event and nothing
// else. No HashMap-tie ambiguity involved.
#[test]
fn uneven_interleave_surfaces_uniquely_longest_buffer() {
    let mut t = BufferTimes::default();
    let frame_ns: u64 = 8_333_333; // 120 Hz
    let mut yields = 0;
    let mut a_events = 0u64;
    for i in 1u64..=24 {
        let (buffer, is_a) = if i % 2 == 0 {
            (0x10, true)
        } else if i % 3 == 0 {
            (0x11, false)
        } else {
            (0x12, false)
        };
        if is_a {
            a_events += 1;
        }
        if let Some(ft) = t.record(FrameRecord { ktime_ns: frame_ns * i, buffer }) {
            yields += 1;
            assert_eq!(ft, Duration::from_nanos(2 * frame_ns));
        }
    }
    assert_eq!(yields, a_events - 1, "every A sighting after the first");
}

// The ring can replay the same record (dt saturates to zero), which
// must not manufacture a phantom frame — and must not disturb the
// ongoing history.
#[test]
fn duplicate_events_do_not_double_count() {
    let mut t = BufferTimes::default();
    let frame_ns: u64 = 16_666_667;
    let fr = |ns: u64| FrameRecord { ktime_ns: ns, buffer: 9 };
    assert!(t.record(fr(frame_ns)).is_none());
    assert_eq!(
        t.record(fr(2 * frame_ns)),
        Some(Duration::from_nanos(frame_ns))
    );
    assert_eq!(t.record(fr(2 * frame_ns)), None);
    assert_eq!(
        t.record(fr(3 * frame_ns)),
        Some(Duration::from_nanos(frame_ns))
    );
}

// History is capped at PER_BUFFER_HISTORY entries; after the cap
// starts recycling oldest slices, steady frames must keep flowing.
#[test]
fn steady_frames_keep_flowing_after_history_cap() {
    let mut t = BufferTimes::default();
    let frame_ns: u64 = 16_666_667;
    let fr = |ns: u64| FrameRecord { ktime_ns: ns, buffer: 5 };
    let events = PER_BUFFER_HISTORY + 40;
    let mut yielded = 0;
    for i in 1..=events {
        if t.record(fr(frame_ns * i as u64)).is_some() {
            yielded += 1;
        }
    }
    assert_eq!(yielded, events - 1);
}

//! Per-PID frame tracker.
//!
//! Surface presentation is triple-buffered: `queueBuffer` is called
//! for up to N back-buffers in flight, and the timestamps come back
//! interleaved. To extract a clean "this frame's time" we group
//! events by the buffer pointer and diff against the previous
//! timestamp from the *same* buffer.
//!
//! To avoid double-counting (each frame would otherwise yield N
//! frametimes), we only surface a frametime when it belongs to the
//! buffer that currently has the longest history *and* shortest
//! cumulative time — the same heuristic upstream uses to pick "the"
//! active buffer.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use crate::error::Result;
use crate::uprobe::QueueBufferProbe;
use crate::wire::FrameRecord;

/// Triple-buffering + one slot of slack, times a sensible upper bound
/// on FPS so a slow consumer doesn't drop history mid-frame.
const PER_BUFFER_HISTORY: usize = 144;

pub struct FrameTracker {
    pub probe: QueueBufferProbe,
    times: BufferTimes,
}

/// Grouping of frametimes by buffer pointer plus the active-buffer
/// heuristic. Kept separate from the probe so the dedup logic is
/// unit-testable without loading a BPF program.
#[derive(Default)]
struct BufferTimes {
    per_buffer: HashMap<u64, BufferHistory>,
}

struct BufferHistory {
    last_ktime_ns: u64,
    durations: VecDeque<Duration>,
}

impl FrameTracker {
    pub fn new(probe: QueueBufferProbe) -> Self {
        Self {
            probe,
            times: BufferTimes::default(),
        }
    }

    /// Pull one record off the ring and, if it represents a new frame
    /// from the currently-active buffer, return its duration.
    pub fn pump(&mut self) -> Result<Option<Duration>> {
        // The `RingBufItem` borrows `ring` (which borrows `self.probe`),
        // so we copy the bytes out and drop the item before calling
        // `self.record(...)` — otherwise the borrow checker (rightly)
        // refuses the second &mut self.
        let record = {
            let mut ring = self.probe.ring_buf()?;
            let Some(item) = ring.next() else {
                return Ok(None);
            };
            if item.len() < size_of::<FrameRecord>() {
                return Ok(None);
            }
            // SAFETY: length checked above.
            unsafe { FrameRecord::from_ring_slice(&item) }
        };
        Ok(self.record(record))
    }

    fn record(&mut self, record: FrameRecord) -> Option<Duration> {
        self.times.record(record)
    }
}

impl BufferTimes {
    fn record(&mut self, record: FrameRecord) -> Option<Duration> {
        let history = self
            .per_buffer
            .entry(record.buffer)
            .or_insert_with(|| BufferHistory {
                last_ktime_ns: record.ktime_ns,
                durations: VecDeque::with_capacity(PER_BUFFER_HISTORY),
            });

        let dt_ns = record.ktime_ns.saturating_sub(history.last_ktime_ns);
        history.last_ktime_ns = record.ktime_ns;
        if dt_ns == 0 {
            // First observation for this buffer — no delta yet.
            return None;
        }
        if history.durations.len() >= PER_BUFFER_HISTORY {
            history.durations.pop_back();
        }
        history.durations.push_front(Duration::from_nanos(dt_ns));

        // Pick the active buffer: among buffers tied for the longest
        // history, the one with the smallest cumulative time is the
        // currently-presenting back-buffer. (The others belong to
        // queued-but-not-yet-shown frames.)
        let max_len = self
            .per_buffer
            .values()
            .map(|h| h.durations.len())
            .max()
            .unwrap_or(0);
        let active_buffer = self
            .per_buffer
            .iter()
            .filter(|(_, h)| h.durations.len() == max_len)
            .min_by_key(|(_, h)| h.durations.iter().copied().sum::<Duration>())
            .map(|(buffer, _)| *buffer)?;

        if active_buffer == record.buffer {
            self.per_buffer.get(&record.buffer)?.durations.front().copied()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

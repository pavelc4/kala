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

    /// Pull records off the ring until a valid active-buffer frametime is
    /// produced, or the ring buffer is exhausted. Returns the frametime and
    /// the number of ring records consumed in this call.
    pub fn pump_counted(&mut self) -> (Option<Duration>, usize) {
        let mut count = 0;
        loop {
            let record = {
                let Some(item) = self.probe.ring().next() else {
                    return (None, count);
                };
                count += 1;
                if item.len() < size_of::<FrameRecord>() {
                    continue;
                }
                // SAFETY: length checked above.
                unsafe { FrameRecord::from_ring_slice(&item) }
            };
            if let Some(duration) = self.record(record) {
                return (Some(duration), count);
            }
        }
    }

    /// Pull records off the ring until a valid active-buffer frametime is
    /// produced, or the ring buffer is exhausted.
    pub fn pump(&mut self) -> Option<Duration> {
        self.pump_counted().0
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
            self.per_buffer
                .get(&record.buffer)?
                .durations
                .front()
                .copied()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;

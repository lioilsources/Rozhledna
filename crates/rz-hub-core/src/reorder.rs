//! Frame reordering and loss handling on the hub (plan 6.4).
//!
//! Frames arrive on independent QUIC streams and may complete out of order. Frames are released
//! strictly in `frame_no` order. If the next frame is missing for longer than
//! `max(2 × frame interval, 40 ms)` or more than 3 frames are queued behind it, the hub jumps to a
//! queued keyframe if there is one, otherwise it requests a keyframe and drops P-frames until one
//! arrives. [`Reorder::floor`] / [`Reorder::waiting_for_keyframe`] let the receive side `stop()` in-flight streams
//! that can no longer be used (T1.5).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum ReorderEvent<T> {
    Ready(T),
    /// Number of frames dropped without decoding.
    Skipped(u32),
    /// Ask the agent for an IDR; carries the last frame number we decoded (or 0).
    RequestKeyframe(u64),
}

pub struct Reorder<T> {
    next: Option<u64>,
    last_done: Option<u64>,
    /// Capture timestamp of the last released frame (0 = unknown).
    last_done_ts: u64,
    /// Capture timestamp of the current epoch's first frame (the DISCONTINUITY one).
    epoch_ts: u64,
    pending: BTreeMap<u64, (bool, u64, T)>,
    /// Frames that look stale by number but are newer than anything released: probably the next epoch whose
    /// DISCONTINUITY frame has not arrived yet (frames complete out of order).
    early: BTreeMap<u64, (bool, u64, T)>,
    waiting_since: Option<Instant>,
    deadline: Duration,
    want_request: bool,
}

impl<T> Reorder<T> {
    pub fn new(frame_interval: Duration) -> Self {
        Self {
            next: None,
            last_done: None,
            last_done_ts: 0,
            epoch_ts: 0,
            pending: BTreeMap::new(),
            early: BTreeMap::new(),
            waiting_since: None,
            deadline: (frame_interval * 2).max(Duration::from_millis(40)),
            want_request: false,
        }
    }

    /// Without timestamps (tests, sources without clocks): epochs are told apart only by DISCONTINUITY.
    pub fn push(&mut self, frame_no: u64, keyframe: bool, discontinuity: bool, item: T, now: Instant) {
        self.push_ts(frame_no, keyframe, discontinuity, 0, item, now)
    }

    /// `ts`: the frame's capture timestamp (agent clock, monotonic). A new epoch (reconnect, resize) restarts frame
    /// numbers; timestamps tell its frames from late frames of the previous epoch.
    pub fn push_ts(&mut self, frame_no: u64, keyframe: bool, discontinuity: bool, ts: u64, item: T, _now: Instant) {
        if discontinuity {
            if ts != 0 && ts < self.epoch_ts {
                return; // the first frame of an older epoch arriving late
            }
            // New epoch: drop everything older, keep frames of this epoch that already arrived.
            self.pending.retain(|_, (_, t, _)| ts != 0 && *t >= ts);
            let early = std::mem::take(&mut self.early);
            for (n, v) in early {
                if ts != 0 && v.1 >= ts {
                    self.pending.insert(n, v);
                }
            }
            self.next = None;
            self.last_done = None;
            self.waiting_since = None;
            self.epoch_ts = ts;
            self.pending.insert(frame_no, (keyframe, ts, item));
            return;
        }
        if ts != 0 && ts < self.epoch_ts {
            return; // late frame of the previous epoch
        }
        let stale = match (self.next, self.last_done) {
            (Some(nx), _) => frame_no < nx,
            (None, Some(ld)) => frame_no <= ld,
            (None, None) => false,
        };
        if !stale {
            self.pending.insert(frame_no, (keyframe, ts, item));
        } else if ts != 0 && ts > self.last_done_ts && self.early.len() < 64 {
            self.early.insert(frame_no, (keyframe, ts, item));
        }
    }

    /// Capture timestamp of the last released frame (0 if unknown): a frame newer than this may belong to a new
    /// epoch even if its number is below [`Reorder::floor`].
    pub fn last_done_ts(&self) -> u64 {
        self.last_done_ts
    }

    /// Frames below this number are useless (already decoded or skipped).
    pub fn floor(&self) -> u64 {
        self.next.or(self.last_done.map(|l| l + 1)).unwrap_or(0)
    }

    /// True while only a keyframe can be used (after a loss or a decoder error); P-frames may be dropped unread.
    pub fn waiting_for_keyframe(&self) -> bool {
        self.next.is_none() && (self.last_done.is_some() || self.want_request)
    }

    /// Last frame released (0 before the first).
    pub fn last_done(&self) -> u64 {
        self.last_done.unwrap_or(0)
    }

    /// The decoder failed: wait for the next keyframe.
    pub fn need_keyframe(&mut self) {
        self.next = None;
        self.want_request = true;
    }

    fn first_key_at_or_after(&self, n: u64) -> Option<u64> {
        self.pending.range(n..).find(|(_, (k, _, _))| *k).map(|(n, _)| *n)
    }

    fn drop_below(&mut self, n: u64) -> u32 {
        let keep = self.pending.split_off(&n);
        let dropped = self.pending.len() as u32;
        self.pending = keep;
        dropped
    }

    pub fn poll(&mut self, now: Instant) -> Vec<ReorderEvent<T>> {
        let mut out = Vec::new();
        let mut skipped = 0u32;
        loop {
            let nx = match self.next {
                Some(nx) => nx,
                None => match self.first_key_at_or_after(0) {
                    Some(k) => {
                        skipped += self.drop_below(k);
                        self.next = Some(k);
                        self.want_request = false;
                        k
                    }
                    None => {
                        // Waiting for a keyframe: P-frames are useless. Keep asking while they arrive.
                        if !self.pending.is_empty() || self.want_request {
                            skipped += self.pending.len() as u32;
                            self.pending.clear();
                            if self.last_done.is_some() || self.want_request {
                                out.push(ReorderEvent::RequestKeyframe(self.last_done.unwrap_or(0)));
                            }
                            self.want_request = false;
                        }
                        break;
                    }
                },
            };
            if let Some((_, ts, item)) = self.pending.remove(&nx) {
                out.push(ReorderEvent::Ready(item));
                self.next = Some(nx + 1);
                self.last_done = Some(nx);
                self.last_done_ts = self.last_done_ts.max(ts);
                self.waiting_since = None;
                continue;
            }
            if self.pending.is_empty() {
                self.waiting_since = None;
                break;
            }
            let since = *self.waiting_since.get_or_insert(now);
            if now.duration_since(since) > self.deadline || self.pending.len() > 3 {
                self.waiting_since = None;
                if let Some(k) = self.first_key_at_or_after(nx) {
                    skipped += (k - nx) as u32 - self.pending.range(nx..k).count() as u32; // missing ones
                    skipped += self.drop_below(k);
                    self.next = Some(k);
                    continue;
                }
                skipped += self.pending.len() as u32 + 1;
                self.pending.clear();
                self.next = None;
                out.push(ReorderEvent::RequestKeyframe(self.last_done.unwrap_or(0)));
            }
            break;
        }
        if skipped > 0 {
            out.push(ReorderEvent::Skipped(skipped));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_epoch_keeps_early_frames_and_drops_late_old_ones() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        // Epoch A: frames 0..=10 at ts 1000.., released.
        for n in 0..=10u64 {
            r.push_ts(n, n == 0, n == 0, 1000 + n, n, t);
        }
        assert_eq!(ready(&r.poll(t)).len(), 11);
        // Epoch B (ts 5000..): frames 1 and 2 complete before frame 0; a late frame 11 of epoch A arrives too.
        r.push_ts(1, false, false, 5001, 101, t);
        r.push_ts(2, false, false, 5002, 102, t);
        r.push_ts(11, false, false, 1011, 11, t); // old epoch, but newer than what was released: kept until B starts
        r.push_ts(0, true, true, 5000, 100, t);
        let ev = r.poll(t);
        assert_eq!(ready(&ev), vec![100, 101, 102], "early frames of the new epoch are not lost");
        assert!(!ev.iter().any(|e| matches!(e, ReorderEvent::RequestKeyframe(_) | ReorderEvent::Skipped(_))));
        r.push_ts(12, false, false, 1012, 12, t); // late frame of epoch A after B started
        r.push_ts(3, false, false, 5003, 103, t);
        assert_eq!(ready(&r.poll(t)), vec![103]);
    }

    fn ready(ev: &[ReorderEvent<u64>]) -> Vec<u64> {
        ev.iter().filter_map(|e| if let ReorderEvent::Ready(n) = e { Some(*n) } else { None }).collect()
    }

    #[test]
    fn in_order_and_out_of_order() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        r.push(0, true, true, 0, t);
        r.push(2, false, false, 2, t);
        assert_eq!(ready(&r.poll(t)), vec![0]);
        r.push(1, false, false, 1, t);
        assert_eq!(ready(&r.poll(t)), vec![1, 2]);
    }

    #[test]
    fn waits_for_first_keyframe() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        r.push(5, false, false, 5, t);
        let ev = r.poll(t);
        assert!(ready(&ev).is_empty());
        r.push(6, true, false, 6, t);
        r.push(7, false, false, 7, t);
        assert_eq!(ready(&r.poll(t)), vec![6, 7]);
    }

    #[test]
    fn gap_times_out_and_requests_keyframe() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        r.push(0, true, false, 0, t);
        r.poll(t);
        r.push(2, false, false, 2, t); // 1 is lost
        assert!(r.poll(t + Duration::from_millis(10)).is_empty());
        // The wait starts at the first poll that sees the gap (t+10); deadline = max(2×16, 40) ms.
        assert!(r.poll(t + Duration::from_millis(45)).is_empty());
        let ev = r.poll(t + Duration::from_millis(55));
        assert!(ev.contains(&ReorderEvent::RequestKeyframe(0)), "{ev:?}");
        // P-frames are dropped until the keyframe.
        r.push(3, false, false, 3, t);
        assert!(ready(&r.poll(t + Duration::from_millis(60))).is_empty());
        r.push(4, true, false, 4, t);
        assert_eq!(ready(&r.poll(t + Duration::from_millis(70))), vec![4]);
    }

    #[test]
    fn jumps_to_queued_keyframe() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        r.push(0, true, false, 0, t);
        r.poll(t);
        for n in [2, 3, 4, 5] {
            r.push(n, n == 4, false, n, t);
        }
        // > 3 queued behind the gap → jump to keyframe 4 without waiting.
        let ev = r.poll(t);
        assert_eq!(ready(&ev), vec![4, 5]);
        assert!(!ev.iter().any(|e| matches!(e, ReorderEvent::RequestKeyframe(_))));
    }

    #[test]
    fn discontinuity_resets() {
        let t = Instant::now();
        let mut r = Reorder::new(Duration::from_millis(16));
        r.push(0, true, false, 0, t);
        r.push(1, false, false, 1, t);
        r.poll(t);
        r.push(0, true, true, 100, t);
        assert_eq!(ready(&r.poll(t)), vec![100]);
    }
}

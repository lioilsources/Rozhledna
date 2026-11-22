//! Monotonic clock and NTP-style clock offset estimation (plan 6.4). Used only for latency measurement.

/// Monotonic time in ns.
///
/// On macOS this is `mach_absolute_time()` in ns, i.e. the same clock as
/// CoreMedia host time, so ScreenCaptureKit timestamps can be compared directly
/// (see [`mach_ticks_to_ns`]).
pub fn now_ns() -> u64 {
    #[cfg(target_os = "macos")]
    {
        mach_ticks_to_ns(unsafe { mach::mach_absolute_time() })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[cfg(target_os = "macos")]
mod mach {
    #[repr(C)]
    pub struct TimebaseInfo {
        pub numer: u32,
        pub denom: u32,
    }
    unsafe extern "C" {
        pub fn mach_absolute_time() -> u64;
        pub fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    }
}

/// Converts `mach_absolute_time` ticks to ns.
#[cfg(target_os = "macos")]
pub fn mach_ticks_to_ns(ticks: u64) -> u64 {
    use std::sync::OnceLock;
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TB.get_or_init(|| {
        let mut info = mach::TimebaseInfo { numer: 0, denom: 0 };
        unsafe { mach::mach_timebase_info(&mut info) };
        (info.numer as u64, info.denom as u64)
    });
    ((ticks as u128 * numer as u128) / denom as u128) as u64
}

/// One Ping/Pong exchange. `t0`,`t3` local clock; `t1`,`t2` remote clock.
#[derive(Debug, Clone, Copy)]
pub struct ClockSample {
    pub t0: u64,
    pub t1: u64,
    pub t2: u64,
    pub t3: u64,
}

impl ClockSample {
    pub fn rtt_ns(&self) -> i64 {
        (self.t3 as i64 - self.t0 as i64) - (self.t2 as i64 - self.t1 as i64)
    }
    /// remote − local.
    pub fn offset_ns(&self) -> i64 {
        ((self.t1 as i64 - self.t0 as i64) + (self.t2 as i64 - self.t3 as i64)) / 2
    }
}

/// Keeps the last N samples and uses the offset of the minimum-RTT one.
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: std::collections::VecDeque<ClockSample>,
}

impl ClockSync {
    const WINDOW: usize = 32;

    pub fn add(&mut self, s: ClockSample) {
        if s.rtt_ns() < 0 {
            return;
        }
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    fn best(&self) -> Option<&ClockSample> {
        self.samples.iter().min_by_key(|s| s.rtt_ns())
    }

    /// remote − local, from the min-RTT sample.
    pub fn offset_ns(&self) -> Option<i64> {
        self.best().map(|s| s.offset_ns())
    }

    pub fn min_rtt_ns(&self) -> Option<i64> {
        self.best().map(|s| s.rtt_ns())
    }

    /// Maps a remote timestamp into the local clock domain.
    pub fn remote_to_local(&self, remote_ns: u64) -> Option<u64> {
        self.offset_ns().map(|o| (remote_ns as i64 - o) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
    }

    #[test]
    fn offset_from_min_rtt() {
        // Remote clock is local + 1_000_000. One-way delay 100 µs, remote processing 10 µs.
        let off = 1_000_000u64;
        let mk = |t0: u64, d_out: u64, d_back: u64| ClockSample {
            t0,
            t1: t0 + d_out + off,
            t2: t0 + d_out + 10_000 + off,
            t3: t0 + d_out + 10_000 + d_back,
        };
        let mut cs = ClockSync::default();
        cs.add(mk(10_000_000, 900_000, 100_000)); // asymmetric, high RTT → ignored
        cs.add(mk(20_000_000, 100_000, 100_000)); // symmetric, min RTT
        assert_eq!(cs.min_rtt_ns(), Some(200_000));
        assert_eq!(cs.offset_ns(), Some(off as i64));
        assert_eq!(cs.remote_to_local(5_000_000), Some(4_000_000));
    }
}

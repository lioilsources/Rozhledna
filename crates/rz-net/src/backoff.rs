//! Reconnect backoff (T1.1): 250 ms → ×2 → max 10 s, ±20 % jitter, reset after a successful connection.

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    cur: Duration,
    rng: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(10))
    }
}

impl Backoff {
    pub fn new(min: Duration, max: Duration) -> Self {
        Self { min, max, cur: min, rng: rz_proto::clock::now_ns() | 1 }
    }

    /// Delay before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        let base = self.cur;
        self.cur = (self.cur * 2).min(self.max);
        // xorshift64: jitter only, no need for a real RNG.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let j = (self.rng % 401) as f64 / 1000.0 - 0.2; // −0.2 … +0.2
        base.mul_f64(1.0 + j)
    }

    pub fn reset(&mut self) {
        self.cur = self.min;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_caps_resets() {
        let mut b = Backoff::default();
        let d: Vec<f64> = (0..8).map(|_| b.next_delay().as_secs_f64()).collect();
        assert!((0.2..=0.3).contains(&d[0]), "{d:?}");
        assert!((0.4..=0.6).contains(&d[1]), "{d:?}");
        assert!(d[7] <= 12.0 && d[7] >= 8.0, "capped at 10 s ±20 %: {d:?}");
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(300));
    }
}

use std::time::{Duration, Instant};

use crate::rtp_::Bitrate;

#[derive(Debug, Clone)]
pub(crate) struct AppRateEwma {
    tau: Duration,
    last_at: Option<Instant>,
    pending_bytes: u64,
    bps: f64,
}

impl AppRateEwma {
    const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(40);

    pub(crate) fn new(tau: Duration) -> Self {
        debug_assert!(!tau.is_zero());
        Self {
            tau,
            last_at: None,
            pending_bytes: 0,
            bps: 0.0,
        }
    }

    pub(crate) fn record_bytes(&mut self, now: Instant, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let Some(last) = self.last_at else {
            self.last_at = Some(now);
            self.pending_bytes = bytes;
            return;
        };
        debug_assert!(now >= last);
        debug_assert!(bytes <= u64::MAX - self.pending_bytes);
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
        let elapsed = now.saturating_duration_since(last);
        if elapsed < Self::MIN_UPDATE_INTERVAL {
            return;
        }
        let instantaneous = self.pending_bytes as f64 * 8.0 / elapsed.as_secs_f64();
        let alpha = 1.0 - (-elapsed.as_secs_f64() / self.tau.as_secs_f64()).exp();
        self.bps = if self.bps == 0.0 {
            instantaneous
        } else {
            self.bps + alpha * (instantaneous - self.bps)
        };
        self.pending_bytes = 0;
        self.last_at = Some(now);
    }

    pub(crate) fn bitrate(&self) -> Bitrate {
        debug_assert!(self.bps.is_finite());
        Bitrate::from(self.bps.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_bytes_produce_a_stable_rate() {
        let start = Instant::now();
        let mut rate = AppRateEwma::new(Duration::from_millis(500));
        for step in 0..=60 {
            rate.record_bytes(start + Duration::from_millis(step * 50), 6_250);
        }

        assert!((rate.bitrate().as_u64() as i64 - 1_000_000).abs() < 60_000);
    }
}

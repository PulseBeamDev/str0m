//! Port of libWebRTC's `LinkCapacityEstimator` from
//! `webrtc/modules/congestion_controller/goog_cc/link_capacity_estimator.{h,cc}`.

use crate::rtp_::Bitrate;

/// Running estimate of link capacity with a normalized variance, used by the AIMD rate control
/// to bound how far a single backoff may move the estimate.
///
/// Unlike a peak tracker this both rises and falls: `on_overuse_detected` folds in the throughput
/// measured at the moment congestion appeared, so a genuinely shrinking link pulls the estimate
/// down rather than pinning it at a stale high-water mark.
#[derive(Debug, Default)]
pub struct AimdLinkCapacity {
    estimate_kbps: Option<f64>,
    deviation_kbps: f64,
}

impl AimdLinkCapacity {
    const ALPHA_OVERUSE: f64 = 0.05;
    const ALPHA_PROBE: f64 = 0.5;
    const DEVIATION_MIN: f64 = 0.4;
    const DEVIATION_MAX: f64 = 2.5;

    pub fn new() -> Self {
        Self {
            estimate_kbps: None,
            deviation_kbps: 0.4,
        }
    }

    pub fn reset(&mut self) {
        self.estimate_kbps = None;
    }

    pub fn estimate(&self) -> Option<Bitrate> {
        self.estimate_kbps
            .map(|kbps| Bitrate::bps((kbps * 1000.0).max(0.0) as u64))
    }

    pub fn on_overuse_detected(&mut self, acknowledged: Bitrate) {
        self.update(acknowledged, Self::ALPHA_OVERUSE);
    }

    pub fn on_probe_rate(&mut self, probe: Bitrate) {
        self.update(probe, Self::ALPHA_PROBE);
    }

    pub fn upper_bound(&self) -> Option<Bitrate> {
        let estimate = self.estimate_kbps?;
        let kbps = estimate + 3.0 * self.deviation_estimate_kbps();
        Some(Bitrate::bps((kbps * 1000.0).max(0.0) as u64))
    }

    fn update(&mut self, sample: Bitrate, alpha: f64) {
        let sample_kbps = sample.as_f64() / 1000.0;
        debug_assert!(sample_kbps.is_finite());
        debug_assert!(sample_kbps >= 0.0);

        let estimate_kbps = match self.estimate_kbps {
            None => sample_kbps,
            Some(current) => (1.0 - alpha) * current + alpha * sample_kbps,
        };
        self.estimate_kbps = Some(estimate_kbps);

        // Estimate the variance of the highest 10% of the samples.
        let norm = estimate_kbps.max(1.0);
        let error_kbps = estimate_kbps - sample_kbps;
        self.deviation_kbps =
            (1.0 - alpha) * self.deviation_kbps + alpha * error_kbps * error_kbps / norm;
        self.deviation_kbps = self
            .deviation_kbps
            .clamp(Self::DEVIATION_MIN, Self::DEVIATION_MAX);

        debug_assert!(self.deviation_kbps.is_finite());
    }

    fn deviation_estimate_kbps(&self) -> f64 {
        let estimate_kbps = self.estimate_kbps.unwrap_or(0.0);
        (self.deviation_kbps * estimate_kbps).sqrt()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn no_estimate_or_upper_bound_without_a_sample() {
        let capacity = AimdLinkCapacity::new();
        assert!(capacity.estimate().is_none());
        assert!(capacity.upper_bound().is_none());
    }

    #[test]
    fn first_sample_initialises_the_estimate() {
        let mut capacity = AimdLinkCapacity::new();
        capacity.on_probe_rate(Bitrate::kbps(1_000));
        assert_eq!(capacity.estimate(), Some(Bitrate::kbps(1_000)));
    }

    #[test]
    fn overuse_samples_pull_the_estimate_down() {
        let mut capacity = AimdLinkCapacity::new();
        capacity.on_probe_rate(Bitrate::kbps(2_000));

        for _ in 0..200 {
            capacity.on_overuse_detected(Bitrate::kbps(500));
        }

        let estimate = capacity.estimate().expect("estimate").as_f64();
        assert!(
            estimate < 600_000.0,
            "sustained overuse at 500kbit/s should drag the estimate down, got {estimate}"
        );
    }

    #[test]
    fn upper_bound_contains_the_estimate() {
        let mut capacity = AimdLinkCapacity::new();
        capacity.on_probe_rate(Bitrate::kbps(1_000));

        let upper = capacity.upper_bound().expect("upper").as_f64();
        let estimate = capacity.estimate().expect("estimate").as_f64();

        assert!(estimate <= upper, "estimate {estimate} <= upper {upper}");
    }
}

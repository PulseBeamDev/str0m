use std::time::{Duration, Instant};

use crate::rtp_::Bitrate;

/// Tracks the link's proven capacity, with a measure of how well it is known.
///
/// Mirrors libWebRTC's `LinkCapacityEstimator`: an exponential moving average of capacity
/// samples, alongside a running variance that widens when samples disagree. The variance is the
/// point. A bare point estimate cannot say how much to trust itself, and a rate controller
/// deciding how far to back off needs exactly that.
///
/// Two kinds of sample, weighted very differently:
///
/// * a probe result is a direct measurement of what the link carried, so it moves the estimate
///   sharply (`PROBE_ALPHA`);
/// * an overuse says the link is carrying less than we thought, but the acknowledged rate at that
///   moment is a poor measure of capacity - it may be application limited, or a transient - so it
///   nudges the estimate rather than replacing it (`OVERUSE_ALPHA`).
///
/// That asymmetry is what lets the estimate serve as a floor for backoff. A one-off overuse while
/// the sender is application limited barely moves it, so a backoff cannot collapse to whatever
/// the application happened to be sending; sustained genuine congestion moves it repeatedly, so
/// the floor follows the link down and does not go stale.
///
/// The previous implementation took the *maximum* of probe results and held it for 60s. That has
/// no notion of confidence and cannot fall: a link degrading while the sender was application
/// limited kept a minute-old high estimate, which is precisely the case where a backoff floor
/// must not be trusted.
pub struct LinkCapacityEstimator {
    /// Current estimate in kbps, if any sample has been seen.
    estimate_kbps: Option<f64>,
    /// Running variance of the estimate, in kbps.
    deviation_kbps: f64,
    /// Time of the most recent sample, for expiry.
    last_estimate_time: Option<Instant>,
}

impl Default for LinkCapacityEstimator {
    fn default() -> Self {
        Self {
            estimate_kbps: None,
            deviation_kbps: 0.4,
            last_estimate_time: None,
        }
    }
}

impl LinkCapacityEstimator {
    /// Duration before an estimate is considered too old to use.
    const DEFAULT_RESET_WINDOW: Duration = Duration::from_secs(60);
    /// Weight of a probe result. Probes measure the link directly, so they move the estimate hard.
    const PROBE_ALPHA: f64 = 0.5;
    /// Weight of an overuse. Small on purpose: see the type comment.
    const OVERUSE_ALPHA: f64 = 0.05;
    /// Bounds on the tracked variance, matching libWebRTC.
    const MIN_DEVIATION_KBPS: f64 = 0.4;
    const MAX_DEVIATION_KBPS: f64 = 2500.0;

    pub fn new() -> Self {
        Self::default()
    }

    /// Record a capacity measured by probing.
    pub fn update_from_probe(&mut self, probe_estimate: Bitrate, now: Instant) {
        if !probe_estimate.is_valid() {
            return;
        }
        self.update(probe_estimate, Self::PROBE_ALPHA, now);
        trace!("Link capacity estimate updated to {} from probe", self.estimate_string());
    }

    /// Record that the link was overusing at `acknowledged`, which is evidence - weak evidence -
    /// that capacity is lower than currently believed.
    pub fn on_overuse_detected(&mut self, acknowledged: Bitrate, now: Instant) {
        if !acknowledged.is_valid() {
            return;
        }
        self.update(acknowledged, Self::OVERUSE_ALPHA, now);
    }

    fn update(&mut self, sample: Bitrate, alpha: f64, now: Instant) {
        let sample_kbps = sample.as_f64() / 1000.0;
        let estimate_kbps = match self.estimate_kbps {
            Some(current) => (1.0 - alpha) * current + alpha * sample_kbps,
            None => sample_kbps,
        };

        // Variance is normalised by the estimate so it stays comparable across link rates: 100
        // kbps of disagreement means something quite different on a 200 kbps link than a 50 Mbps
        // one.
        let norm = estimate_kbps.max(1.0);
        let error_kbps = estimate_kbps - sample_kbps;
        self.deviation_kbps = ((1.0 - alpha) * self.deviation_kbps
            + alpha * error_kbps * error_kbps / norm)
            .clamp(Self::MIN_DEVIATION_KBPS, Self::MAX_DEVIATION_KBPS);

        self.estimate_kbps = Some(estimate_kbps);
        self.last_estimate_time = Some(now);
    }

    fn deviation_estimate_kbps(&self) -> f64 {
        // The variance is normalised by the estimate, so undo that to get a standard deviation.
        (self.deviation_kbps * self.estimate_kbps.unwrap_or(0.0)).sqrt()
    }

    fn fresh(&self, now: Instant) -> bool {
        self.last_estimate_time
            .is_some_and(|t| now.saturating_duration_since(t) <= Self::DEFAULT_RESET_WINDOW)
    }

    /// The current estimate, if one exists and has not expired.
    pub fn capacity_estimate(&self, now: Instant) -> Option<Bitrate> {
        if !self.fresh(now) {
            if self.estimate_kbps.is_some() {
                trace!("Link capacity estimate expired");
            }
            return None;
        }
        self.estimate_kbps
            .map(|kbps| Bitrate::from(kbps * 1000.0))
    }

    /// The low end of what the link is believed to carry: three standard deviations below the
    /// estimate.
    ///
    /// This is what a backoff should be bounded by rather than the estimate itself. Being
    /// conservative in exactly the right direction, it declines to hold the rate up when the
    /// measurement is uncertain, and only asserts a floor when capacity is well established.
    pub fn lower_bound(&self, now: Instant) -> Option<Bitrate> {
        if !self.fresh(now) {
            return None;
        }
        let estimate = self.estimate_kbps?;
        let lower = (estimate - 3.0 * self.deviation_estimate_kbps()).max(0.0);
        Some(Bitrate::from(lower * 1000.0))
    }

    fn estimate_string(&self) -> String {
        self.estimate_kbps
            .map(|k| format!("{:.0} kbps", k))
            .unwrap_or_else(|| "none".to_string())
    }

    /// Reset the capacity estimate.
    #[cfg(test)]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether any estimate has been recorded.
    #[cfg(test)]
    pub fn has_estimate(&self) -> bool {
        self.estimate_kbps.is_some() && self.last_estimate_time.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kbps(estimator: &LinkCapacityEstimator, now: Instant) -> f64 {
        estimator.capacity_estimate(now).unwrap().as_f64() / 1000.0
    }

    #[test]
    fn starts_with_no_estimate() {
        let estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        assert_eq!(estimator.capacity_estimate(now), None);
        assert_eq!(estimator.lower_bound(now), None);
        assert!(!estimator.has_estimate());
    }

    #[test]
    fn adopts_the_first_probe_outright() {
        let mut estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        estimator.update_from_probe(Bitrate::mbps(10), now);

        assert!((kbps(&estimator, now) - 10_000.0).abs() < 1.0);
        assert!(estimator.has_estimate());
    }

    /// Probes move the estimate quickly in either direction. The previous implementation kept the
    /// maximum, so a link that had genuinely slowed could never be reflected.
    #[test]
    fn probes_move_the_estimate_down_as_well_as_up() {
        let mut estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        estimator.update_from_probe(Bitrate::mbps(10), now);
        estimator.update_from_probe(Bitrate::mbps(5), now);

        let after = kbps(&estimator, now);
        assert!(
            after < 9_000.0 && after > 5_000.0,
            "a 5 Mbps probe should pull a 10 Mbps estimate well down, got {after} kbps"
        );
    }

    /// The asymmetry that lets the estimate serve as a backoff floor: one overuse must barely
    /// move it, so an application-limited blip cannot collapse the floor.
    #[test]
    fn a_single_overuse_barely_moves_the_estimate() {
        let mut estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        estimator.update_from_probe(Bitrate::mbps(3), now);
        estimator.on_overuse_detected(Bitrate::kbps(140), now);

        let after = kbps(&estimator, now);
        assert!(
            after > 2_800.0,
            "one overuse at an application-limited 140 kbps should not move a 3 Mbps estimate \
             far, got {after} kbps"
        );
    }

    /// ...and the other half: sustained overuse must walk it down, so the floor cannot go stale
    /// on a link that has genuinely degraded.
    #[test]
    fn sustained_overuse_walks_the_estimate_down() {
        let mut estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        estimator.update_from_probe(Bitrate::mbps(3), now);
        for _ in 0..100 {
            estimator.on_overuse_detected(Bitrate::kbps(800), now);
        }

        let after = kbps(&estimator, now);
        assert!(
            after < 1_200.0,
            "a hundred overuses at 800 kbps should bring a 3 Mbps estimate down near it, got \
             {after} kbps"
        );
    }

    /// Disagreement widens the variance, which lowers the bound a backoff is allowed to trust.
    #[test]
    fn the_lower_bound_widens_when_samples_disagree() {
        let now = Instant::now();

        let mut steady = LinkCapacityEstimator::new();
        for _ in 0..10 {
            steady.update_from_probe(Bitrate::mbps(3), now);
        }

        let mut noisy = LinkCapacityEstimator::new();
        for i in 0..10 {
            let sample = if i % 2 == 0 {
                Bitrate::mbps(5)
            } else {
                Bitrate::mbps(1)
            };
            noisy.update_from_probe(sample, now);
        }

        let steady_gap = steady.capacity_estimate(now).unwrap().as_f64()
            - steady.lower_bound(now).unwrap().as_f64();
        let noisy_gap = noisy.capacity_estimate(now).unwrap().as_f64()
            - noisy.lower_bound(now).unwrap().as_f64();

        assert!(
            noisy_gap > steady_gap,
            "disagreeing samples should produce a wider band than consistent ones: \
             noisy {noisy_gap} vs steady {steady_gap}"
        );
    }

    #[test]
    fn estimate_expires_after_reset_window() {
        let mut estimator = LinkCapacityEstimator::new();
        let now = Instant::now();

        estimator.update_from_probe(Bitrate::mbps(10), now);
        assert!(estimator.capacity_estimate(now).is_some());

        let later = now + LinkCapacityEstimator::DEFAULT_RESET_WINDOW + Duration::from_secs(1);
        assert_eq!(estimator.capacity_estimate(later), None);
        assert_eq!(estimator.lower_bound(later), None);
    }
}

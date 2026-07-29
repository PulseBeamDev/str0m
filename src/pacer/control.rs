use crate::rtp_::Bitrate;

const PACING_FACTOR: f64 = 1.1;

pub(crate) struct PacingResult {
    /// The bitrate at which the pacer may emit padding **when there is no media queued**.
    ///
    /// This value is intentionally an *absolute* target used by the pacer, not a “delta to add
    /// on top of current media”. In practice the **effective padding** over time is the
    /// difference between this target and the media actually sent, because padding is only used
    /// to fill gaps (empty queues), not to continuously top-up while media is flowing.
    pub padding_rate: Bitrate,
    pub pacing_rate: Bitrate,
}

/// Controls the pacing and padding rates.
pub(crate) struct PacerControl {}

impl PacerControl {
    pub fn new() -> Self {
        Self {}
    }

    pub fn calculate(
        &self,
        current_bitrate: Bitrate,
        desired_bitrate: Bitrate,
        estimate: Bitrate,
        is_overuse: bool,
    ) -> PacingResult {
        let padding_rate = if is_overuse {
            Bitrate::ZERO
        } else if !current_bitrate.is_zero() {
            estimate.min(desired_bitrate)
        } else {
            Bitrate::ZERO
        };

        // Set pacing rate to smooth out media transmission (burst avoidance).
        // Must be at least the current BWE estimate * factor, but also high enough
        // to allow the padding we want to send.
        let min_pacing_rate = estimate * PACING_FACTOR;
        let pacing_rate = min_pacing_rate.max(padding_rate);

        PacingResult {
            padding_rate,
            pacing_rate,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn padding_enabled_with_active_media() {
        let c = PacerControl::new();
        let estimate = Bitrate::kbps(1_000);

        let r = c.calculate(Bitrate::kbps(500), Bitrate::kbps(1_500), estimate, false);

        assert_eq!(r.padding_rate, estimate);
    }

    #[test]
    fn no_padding_without_active_media() {
        let c = PacerControl::new();
        let estimate = Bitrate::kbps(1_000);

        let r = c.calculate(Bitrate::ZERO, Bitrate::kbps(1_500), estimate, false);

        assert_eq!(r.padding_rate, Bitrate::ZERO);
    }

    #[test]
    fn overuse_suppresses_padding() {
        let c = PacerControl::new();
        let estimate = Bitrate::mbps(40);

        let r = c.calculate(Bitrate::mbps(10), Bitrate::mbps(50), estimate, true);
        assert_eq!(r.padding_rate, Bitrate::ZERO);
    }

    #[test]
    fn padding_does_not_exceed_desired_bitrate() {
        let c = PacerControl::new();
        let desired = Bitrate::kbps(750);

        let r = c.calculate(Bitrate::kbps(500), desired, Bitrate::kbps(1_000), false);

        assert_eq!(r.padding_rate, desired);
    }
}

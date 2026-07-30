use crate::rtp_::Bitrate;

const PACING_FACTOR: f64 = 1.1;

pub(crate) struct PacingResult {
    pub padding_rate: Bitrate,
    pub pacing_rate: Bitrate,
}

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
    ) -> PacingResult {
        let padding_target = estimate.min(desired_bitrate);
        let padding_rate = if current_bitrate.is_zero() {
            Bitrate::ZERO
        } else {
            padding_target
        };

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

        let r = c.calculate(Bitrate::kbps(500), Bitrate::kbps(1_500), estimate);

        assert_eq!(r.padding_rate, estimate);
    }

    #[test]
    fn no_padding_without_active_media() {
        let c = PacerControl::new();
        let estimate = Bitrate::kbps(1_000);

        let r = c.calculate(Bitrate::ZERO, Bitrate::kbps(1_500), estimate);

        assert_eq!(r.padding_rate, Bitrate::ZERO);
    }

    #[test]
    fn padding_target_does_not_exceed_desired_bitrate() {
        let c = PacerControl::new();
        let desired = Bitrate::kbps(750);

        let r = c.calculate(Bitrate::kbps(500), desired, Bitrate::kbps(1_000));

        assert_eq!(r.padding_rate, desired);
    }

    #[test]
    fn padding_target_is_independent_of_current_media_rate() {
        let c = PacerControl::new();

        let r = c.calculate(Bitrate::kbps(900), Bitrate::kbps(750), Bitrate::kbps(1_000));

        assert_eq!(r.padding_rate, Bitrate::kbps(750));
    }
}

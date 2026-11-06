use rusplay_client::{query_display, resolve_target};
use rusplay_config::DisplayTargetConfig;
use rusplay_core::{ColorPrimaries, TransferFunction};

const NITS_LIMIT: f64 = 10_000.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayColorDefaults {
    primaries: String,
    transfer: String,
    reference_white: Option<String>,
    peak: Option<String>,
    black: Option<String>,
}

impl DisplayColorDefaults {
    pub fn query() -> Option<Self> {
        let image = query_display(None).ok()?;
        let volume = resolve_target(&image, &DisplayTargetConfig::default()).ok()?;

        Some(Self::from_description(
            volume.primaries,
            volume.transfer,
            volume.reference_white_nits,
            volume.peak_nits,
            volume.black_nits,
        ))
    }

    pub fn from_description(
        primaries: ColorPrimaries,
        transfer: TransferFunction,
        reference_white_nits: f64,
        peak_nits: f64,
        black_nits: f64,
    ) -> Self {
        Self {
            primaries: choice_label(primaries),
            transfer: choice_label(transfer),
            reference_white: whole_nits_label(reference_white_nits),
            peak: whole_nits_label(peak_nits),
            black: black_label(black_nits),
        }
    }

    pub fn label(&self, path: &str) -> Option<&str> {
        match path {
            "color.target.primaries" => Some(&self.primaries),
            "color.target.transfer" => Some(&self.transfer),
            "color.target.reference_white_nits" => self.reference_white.as_deref(),
            "color.target.peak_nits" => self.peak.as_deref(),
            "color.target.black_nits" => self.black.as_deref(),
            _ => None,
        }
    }
}

fn choice_label(name: impl std::fmt::Display) -> String {
    format!("{name} (display)")
}

fn whole_nits_label(nits: f64) -> Option<String> {
    let whole = whole_nits(nits)?;

    Some(format!("{whole} (display)"))
}

fn black_label(nits: f64) -> Option<String> {
    if !nits.is_finite() || !(0.0..NITS_LIMIT).contains(&nits) {
        return None;
    }

    Some(format!("{} (display)", nits_text(nits)))
}

fn whole_nits(nits: f64) -> Option<u32> {
    if !nits.is_finite() {
        return None;
    }

    let rounded = nits.round();
    if !(1.0..=NITS_LIMIT).contains(&rounded) {
        return None;
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(rounded as u32)
}

fn nits_text(nits: f64) -> String {
    if let Some(whole) = whole_nits(nits)
        && (nits - f64::from(whole)).abs() < 0.00005
    {
        return whole.to_string();
    }

    let text = format!("{nits:.4}");

    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_srgb_display_labels_the_unset_target_fields() {
        let defaults = DisplayColorDefaults::from_description(
            ColorPrimaries::Bt709,
            TransferFunction::Srgb,
            203.0,
            203.2,
            0.0,
        );

        assert_eq!(
            defaults.label("color.target.primaries"),
            Some("bt709 (display)")
        );
        assert_eq!(
            defaults.label("color.target.transfer"),
            Some("srgb (display)")
        );
        assert_eq!(
            defaults.label("color.target.reference_white_nits"),
            Some("203 (display)")
        );
        assert_eq!(
            defaults.label("color.target.peak_nits"),
            Some("203 (display)")
        );
        assert_eq!(
            defaults.label("color.target.black_nits"),
            Some("0 (display)")
        );
        assert_eq!(defaults.label("video.color.primaries"), None);
    }

    #[test]
    fn a_fractional_black_level_keeps_its_digits() {
        let defaults = DisplayColorDefaults::from_description(
            ColorPrimaries::DisplayP3,
            TransferFunction::Pq,
            203.0,
            600.0,
            0.005,
        );

        assert_eq!(
            defaults.label("color.target.primaries"),
            Some("display-p3 (display)")
        );
        assert_eq!(
            defaults.label("color.target.transfer"),
            Some("pq (display)")
        );
        assert_eq!(
            defaults.label("color.target.black_nits"),
            Some("0.005 (display)")
        );
    }

    #[test]
    fn luminance_outside_the_settings_range_is_left_unset() {
        let defaults = DisplayColorDefaults::from_description(
            ColorPrimaries::Bt2020,
            TransferFunction::Hlg,
            0.0,
            20_000.0,
            -1.0,
        );

        assert_eq!(defaults.label("color.target.reference_white_nits"), None);
        assert_eq!(defaults.label("color.target.peak_nits"), None);
        assert_eq!(defaults.label("color.target.black_nits"), None);
    }
}

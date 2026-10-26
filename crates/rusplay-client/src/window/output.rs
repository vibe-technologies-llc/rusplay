use rusplay_config::{DisplayOutputConfig, DitherMode, OutputColorSpace, OutputFormat};
use rusplay_core::{ColorPrimaries, HdrLevels, TransferFunction};
use rusplay_scale::ColorVolume;

use crate::ClientError;

pub(crate) const PQ_SIGNAL_REFERENCE_NITS: f64 = HdrLevels::BT2408_REFERENCE_WHITE_NITS as f64;

const EIGHT_BIT_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Rgba8Unorm,
];
const TEN_BIT_FORMATS: [wgpu::TextureFormat; 1] = [wgpu::TextureFormat::Rgb10a2Unorm];
const FLOAT_FORMATS: [wgpu::TextureFormat; 1] = [wgpu::TextureFormat::Rgba16Float];
const INTEGER_FORMATS: [wgpu::TextureFormat; 3] = [
    wgpu::TextureFormat::Rgb10a2Unorm,
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Rgba8Unorm,
];
const PQ_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Rgb10a2Unorm,
    wgpu::TextureFormat::Rgba16Float,
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Display {
    Managed(ColorVolume),
    Unmanaged(ColorVolume),
}

impl Display {
    const fn volume(&self) -> &ColorVolume {
        match self {
            Self::Managed(volume) | Self::Unmanaged(volume) => volume,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Signal {
    Linear,
    Srgb,
    Bt709,
    Gamma22,
    ExtendedLinear,
    Pq { white_nits: f64 },
    Hlg { white_nits: f64, peak_nits: f64 },
}

impl Signal {
    fn of_volume(volume: &ColorVolume) -> Self {
        match volume.transfer {
            TransferFunction::Linear => Self::Linear,
            TransferFunction::Srgb => Self::Srgb,
            TransferFunction::Bt709 => Self::Bt709,
            TransferFunction::Gamma22 => Self::Gamma22,
            TransferFunction::Pq => Self::Pq {
                white_nits: volume.reference_white_nits,
            },
            TransferFunction::Hlg => Self::Hlg {
                white_nits: volume.reference_white_nits,
                peak_nits: volume.peak_nits,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DitherPattern {
    Ordered,
    BlueNoise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dither {
    pub pattern: DitherPattern,
    pub bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayEncoding {
    pub target: ColorVolume,
    pub primaries: ColorPrimaries,
    pub signal: Signal,
    pub dither: Option<Dither>,
}

impl DisplayEncoding {
    pub(crate) fn passthrough(target: ColorVolume, dither: Option<Dither>) -> Self {
        Self {
            target,
            primaries: target.primaries,
            signal: Signal::of_volume(&target),
            dither,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SurfaceOutput {
    pub format: wgpu::TextureFormat,
    pub color_space: wgpu::SurfaceColorSpace,
    pub encoding: DisplayEncoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Space {
    Srgb,
    DisplayP3,
    ExtendedLinear,
    Pq,
}

impl Space {
    const fn surface(self) -> wgpu::SurfaceColorSpace {
        match self {
            Self::Srgb => wgpu::SurfaceColorSpace::Srgb,
            Self::DisplayP3 => wgpu::SurfaceColorSpace::DisplayP3,
            Self::ExtendedLinear => wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
            Self::Pq => wgpu::SurfaceColorSpace::Bt2100Pq,
        }
    }

    const fn formats(self) -> &'static [wgpu::TextureFormat] {
        match self {
            Self::Srgb | Self::DisplayP3 => &INTEGER_FORMATS,
            Self::ExtendedLinear => &FLOAT_FORMATS,
            Self::Pq => &PQ_FORMATS,
        }
    }

    fn candidates(config: OutputColorSpace, display: &Display) -> Vec<Self> {
        match config {
            OutputColorSpace::Srgb => vec![Self::Srgb],
            OutputColorSpace::DisplayP3 => vec![Self::DisplayP3],
            OutputColorSpace::ExtendedLinear => vec![Self::ExtendedLinear],
            OutputColorSpace::Pq => vec![Self::Pq],
            OutputColorSpace::Auto => match display {
                Display::Managed(volume) if volume.is_hdr() => {
                    vec![Self::Pq, Self::ExtendedLinear, Self::Srgb]
                }
                Display::Managed(volume) if volume.primaries == ColorPrimaries::DisplayP3 => {
                    vec![Self::DisplayP3, Self::ExtendedLinear, Self::Srgb]
                }
                Display::Managed(volume) if volume.primaries != ColorPrimaries::Bt709 => {
                    vec![Self::ExtendedLinear, Self::Srgb]
                }
                Display::Managed(_) | Display::Unmanaged(_) => vec![Self::Srgb],
            },
        }
    }

    fn encoding(self, display: &Display, dither: Option<Dither>) -> DisplayEncoding {
        let volume = display.volume();
        let white = volume.reference_white_nits;

        match (self, display) {
            (Self::Srgb, Display::Unmanaged(volume)) => {
                DisplayEncoding::passthrough(*volume, dither)
            }
            (Self::Srgb, Display::Managed(_)) => DisplayEncoding {
                target: sdr_volume(ColorPrimaries::Bt709, white),
                primaries: ColorPrimaries::Bt709,
                signal: Signal::Srgb,
                dither,
            },
            (Self::DisplayP3, _) => DisplayEncoding {
                target: sdr_volume(ColorPrimaries::DisplayP3, white),
                primaries: ColorPrimaries::DisplayP3,
                signal: Signal::Srgb,
                dither,
            },
            (Self::ExtendedLinear, _) => DisplayEncoding {
                target: *volume,
                primaries: ColorPrimaries::Bt709,
                signal: Signal::ExtendedLinear,
                dither,
            },
            (Self::Pq, _) => DisplayEncoding {
                target: hdr_volume(volume),
                primaries: ColorPrimaries::Bt2020,
                signal: Signal::Pq {
                    white_nits: PQ_SIGNAL_REFERENCE_NITS,
                },
                dither,
            },
        }
    }
}

fn sdr_volume(primaries: ColorPrimaries, white: f64) -> ColorVolume {
    ColorVolume {
        primaries,
        transfer: TransferFunction::Srgb,
        reference_white_nits: white,
        peak_nits: white,
        black_nits: 0.0,
    }
}

fn hdr_volume(volume: &ColorVolume) -> ColorVolume {
    let peak_nits = if volume.is_hdr() {
        volume.peak_nits
    } else {
        f64::from(HdrLevels::default().peak_nits).max(volume.reference_white_nits)
    };

    ColorVolume {
        transfer: TransferFunction::Pq,
        peak_nits,
        black_nits: volume.black_nits.min(peak_nits / 2.0),
        ..*volume
    }
}

fn configured_formats(format: OutputFormat, space: Space) -> &'static [wgpu::TextureFormat] {
    match format {
        OutputFormat::Auto => space.formats(),
        OutputFormat::Rgba8 => &EIGHT_BIT_FORMATS,
        OutputFormat::Rgb10a2 => &TEN_BIT_FORMATS,
        OutputFormat::Rgba16Float => &FLOAT_FORMATS,
    }
}

fn dither_for(mode: DitherMode, format: wgpu::TextureFormat) -> Option<Dither> {
    let pattern = match mode {
        DitherMode::BlueNoise => DitherPattern::BlueNoise,
        DitherMode::Ordered => DitherPattern::Ordered,
        DitherMode::Off => return None,
    };
    let bits = match format {
        wgpu::TextureFormat::Rgb10a2Unorm => 10,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => 8,
        _ => return None,
    };

    Some(Dither { pattern, bits })
}

fn offers(
    capabilities: &[wgpu::SurfaceFormatCapabilities],
    format: wgpu::TextureFormat,
    space: Space,
) -> bool {
    let Some(wanted) = space.surface().to_color_spaces() else {
        return false;
    };

    capabilities
        .iter()
        .any(|offered| offered.format == format && offered.color_spaces.contains(wanted))
}

impl SurfaceOutput {
    pub(crate) fn choose(
        capabilities: &[wgpu::SurfaceFormatCapabilities],
        display: &Display,
        config: DisplayOutputConfig,
    ) -> Result<Self, ClientError> {
        Space::candidates(config.color_space, display)
            .into_iter()
            .find_map(|space| {
                configured_formats(config.format, space)
                    .iter()
                    .copied()
                    .find(|format| offers(capabilities, *format, space))
                    .map(|format| Self {
                        format,
                        color_space: space.surface(),
                        encoding: space.encoding(display, dither_for(config.dither, format)),
                    })
            })
            .ok_or(ClientError::SurfaceOutputUnsupported {
                color_space: config.color_space,
                format: config.format,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(
        entries: &[(wgpu::TextureFormat, wgpu::SurfaceColorSpaces)],
    ) -> Vec<wgpu::SurfaceFormatCapabilities> {
        entries
            .iter()
            .map(|(format, color_spaces)| wgpu::SurfaceFormatCapabilities {
                format: *format,
                color_spaces: *color_spaces,
            })
            .collect()
    }

    fn plain_surface() -> Vec<wgpu::SurfaceFormatCapabilities> {
        offered(&[
            (
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::SurfaceColorSpaces::SRGB,
            ),
            (
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::SurfaceColorSpaces::SRGB,
            ),
        ])
    }

    fn managed_surface() -> Vec<wgpu::SurfaceFormatCapabilities> {
        offered(&[
            (
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::SurfaceColorSpaces::SRGB | wgpu::SurfaceColorSpaces::DISPLAY_P3,
            ),
            (
                wgpu::TextureFormat::Rgb10a2Unorm,
                wgpu::SurfaceColorSpaces::SRGB
                    | wgpu::SurfaceColorSpaces::DISPLAY_P3
                    | wgpu::SurfaceColorSpaces::BT2100_PQ,
            ),
            (
                wgpu::TextureFormat::Rgba16Float,
                wgpu::SurfaceColorSpaces::SRGB | wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR,
            ),
        ])
    }

    fn hdr_display() -> ColorVolume {
        ColorVolume {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            reference_white_nits: 250.0,
            peak_nits: 800.0,
            black_nits: 0.01,
        }
    }

    #[test]
    fn an_sdr_display_gets_the_deepest_srgb_swapchain_with_dithering() {
        let display = Display::Managed(ColorVolume::srgb_display());
        let config = DisplayOutputConfig::default();

        let deep = SurfaceOutput::choose(&managed_surface(), &display, config).unwrap();
        let plain = SurfaceOutput::choose(&plain_surface(), &display, config).unwrap();

        assert_eq!(deep.format, wgpu::TextureFormat::Rgb10a2Unorm);
        assert_eq!(deep.color_space, wgpu::SurfaceColorSpace::Srgb);
        assert_eq!(deep.encoding.signal, Signal::Srgb);
        assert_eq!(deep.encoding.target, ColorVolume::srgb_display());
        assert_eq!(
            deep.encoding.dither,
            Some(Dither {
                pattern: DitherPattern::BlueNoise,
                bits: 10,
            })
        );
        assert_eq!(plain.format, wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(plain.encoding.dither.map(|dither| dither.bits), Some(8));
    }

    #[test]
    fn an_hdr_display_gets_pq_and_falls_back_to_scrgb_then_srgb() {
        let display = Display::Managed(hdr_display());
        let config = DisplayOutputConfig::default();
        let without_pq = offered(&[(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::SurfaceColorSpaces::SRGB | wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR,
        )]);

        let pq = SurfaceOutput::choose(&managed_surface(), &display, config).unwrap();
        let scrgb = SurfaceOutput::choose(&without_pq, &display, config).unwrap();
        let sdr = SurfaceOutput::choose(&plain_surface(), &display, config).unwrap();

        assert_eq!(pq.format, wgpu::TextureFormat::Rgb10a2Unorm);
        assert_eq!(pq.color_space, wgpu::SurfaceColorSpace::Bt2100Pq);
        assert_eq!(pq.encoding.target, hdr_display());
        assert_eq!(pq.encoding.primaries, ColorPrimaries::Bt2020);
        assert_eq!(
            pq.encoding.signal,
            Signal::Pq {
                white_nits: PQ_SIGNAL_REFERENCE_NITS
            }
        );
        assert_eq!(
            scrgb.color_space,
            wgpu::SurfaceColorSpace::ExtendedSrgbLinear
        );
        assert_eq!(scrgb.encoding.signal, Signal::ExtendedLinear);
        assert_eq!(scrgb.encoding.primaries, ColorPrimaries::Bt709);
        assert_eq!(scrgb.encoding.target, hdr_display());
        assert_eq!(scrgb.encoding.dither, None);
        assert_eq!(sdr.color_space, wgpu::SurfaceColorSpace::Srgb);
        assert_eq!(
            sdr.encoding.target,
            sdr_volume(ColorPrimaries::Bt709, 250.0)
        );
    }

    #[test]
    fn a_wide_gamut_sdr_display_gets_display_p3() {
        let display = Display::Managed(sdr_volume(ColorPrimaries::DisplayP3, 203.0));

        let output =
            SurfaceOutput::choose(&managed_surface(), &display, DisplayOutputConfig::default())
                .unwrap();

        assert_eq!(output.color_space, wgpu::SurfaceColorSpace::DisplayP3);
        assert_eq!(output.format, wgpu::TextureFormat::Rgb10a2Unorm);
        assert_eq!(output.encoding.primaries, ColorPrimaries::DisplayP3);
        assert_eq!(output.encoding.target.primaries, ColorPrimaries::DisplayP3);
    }

    #[test]
    fn an_unmanaged_display_is_encoded_as_its_overrides_describe() {
        let display = Display::Unmanaged(hdr_display());

        let output =
            SurfaceOutput::choose(&managed_surface(), &display, DisplayOutputConfig::default())
                .unwrap();

        assert_eq!(output.color_space, wgpu::SurfaceColorSpace::Srgb);
        assert_eq!(output.encoding.target, hdr_display());
        assert_eq!(output.encoding.primaries, ColorPrimaries::Bt2020);
        assert_eq!(output.encoding.signal, Signal::Pq { white_nits: 250.0 });
    }

    #[test]
    fn a_forced_choice_the_surface_lacks_is_an_error() {
        let display = Display::Managed(ColorVolume::srgb_display());
        let config = DisplayOutputConfig {
            color_space: OutputColorSpace::Pq,
            format: OutputFormat::Rgba8,
            ..DisplayOutputConfig::default()
        };
        let float = DisplayOutputConfig {
            format: OutputFormat::Rgba16Float,
            dither: DitherMode::Ordered,
            ..DisplayOutputConfig::default()
        };

        let forced = SurfaceOutput::choose(&managed_surface(), &display, float).unwrap();

        assert!(matches!(
            SurfaceOutput::choose(&managed_surface(), &display, config),
            Err(ClientError::SurfaceOutputUnsupported {
                color_space: OutputColorSpace::Pq,
                format: OutputFormat::Rgba8,
            })
        ));
        assert_eq!(forced.format, wgpu::TextureFormat::Rgba16Float);
        assert_eq!(forced.color_space, wgpu::SurfaceColorSpace::Srgb);
        assert_eq!(forced.encoding.dither, None);
    }
}

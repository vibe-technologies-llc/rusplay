use pipewire::spa::sys;
use rusplay_core::{
    Chromaticities, ColorPrimaries, ColorRange, ColorSpace, HdrLevels, MatrixCoefficients,
    TransferFunction,
};

use crate::CaptureError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SpaColorimetry {
    pub range: u32,
    pub matrix: u32,
    pub transfer: u32,
    pub primaries: u32,
}

pub(crate) fn describe_color(
    colorimetry: &SpaColorimetry,
    hdr: HdrLevels,
) -> Result<ColorSpace, CaptureError> {
    let transfer = transfer(colorimetry.transfer)?;
    let primaries = primaries(colorimetry.primaries, transfer)?;

    Ok(ColorSpace {
        primaries,
        transfer,
        matrix: matrix(colorimetry.matrix),
        range: range(colorimetry.range),
        hdr,
    })
}

fn transfer(id: u32) -> Result<TransferFunction, CaptureError> {
    match id {
        sys::SPA_VIDEO_TRANSFER_UNKNOWN | sys::SPA_VIDEO_TRANSFER_SRGB => {
            Ok(TransferFunction::Srgb)
        }
        sys::SPA_VIDEO_TRANSFER_GAMMA10 => Ok(TransferFunction::Linear),
        sys::SPA_VIDEO_TRANSFER_GAMMA22 | sys::SPA_VIDEO_TRANSFER_ADOBERGB => {
            Ok(TransferFunction::Gamma22)
        }
        sys::SPA_VIDEO_TRANSFER_BT709
        | sys::SPA_VIDEO_TRANSFER_BT601
        | sys::SPA_VIDEO_TRANSFER_SMPTE240M
        | sys::SPA_VIDEO_TRANSFER_BT2020_10
        | sys::SPA_VIDEO_TRANSFER_BT2020_12 => Ok(TransferFunction::Bt709),
        sys::SPA_VIDEO_TRANSFER_SMPTE2084 => Ok(TransferFunction::Pq),
        sys::SPA_VIDEO_TRANSFER_ARIB_STD_B67 => Ok(TransferFunction::Hlg),
        spa_transfer => Err(CaptureError::TransferUnsupported { spa_transfer }),
    }
}

fn primaries(id: u32, transfer: TransferFunction) -> Result<ColorPrimaries, CaptureError> {
    let named = match id {
        sys::SPA_VIDEO_COLOR_PRIMARIES_UNKNOWN if transfer.is_hdr() => ColorPrimaries::Bt2020,
        sys::SPA_VIDEO_COLOR_PRIMARIES_UNKNOWN | sys::SPA_VIDEO_COLOR_PRIMARIES_BT709 => {
            ColorPrimaries::Bt709
        }
        sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020 => ColorPrimaries::Bt2020,
        sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTE170M | sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTE240M => {
            ColorPrimaries::Bt601_525
        }
        sys::SPA_VIDEO_COLOR_PRIMARIES_BT470BG => ColorPrimaries::Bt601_625,
        sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTERP431 => ColorPrimaries::DciP3,
        sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTEEG432 => ColorPrimaries::DisplayP3,
        sys::SPA_VIDEO_COLOR_PRIMARIES_ADOBERGB => {
            ColorPrimaries::Custom(Chromaticities::ADOBE_RGB)
        }
        sys::SPA_VIDEO_COLOR_PRIMARIES_BT470M => ColorPrimaries::Custom(Chromaticities::BT470M),
        sys::SPA_VIDEO_COLOR_PRIMARIES_FILM => ColorPrimaries::Custom(Chromaticities::GENERIC_FILM),
        sys::SPA_VIDEO_COLOR_PRIMARIES_EBU3213 => ColorPrimaries::Custom(Chromaticities::EBU3213),
        spa_primaries => return Err(CaptureError::PrimariesUnsupported { spa_primaries }),
    };

    Ok(named)
}

fn matrix(id: u32) -> Option<MatrixCoefficients> {
    match id {
        sys::SPA_VIDEO_COLOR_MATRIX_BT709 => Some(MatrixCoefficients::Bt709),
        sys::SPA_VIDEO_COLOR_MATRIX_BT601 => Some(MatrixCoefficients::Bt601),
        sys::SPA_VIDEO_COLOR_MATRIX_BT2020 => Some(MatrixCoefficients::Bt2020Ncl),
        _ => None,
    }
}

fn range(id: u32) -> ColorRange {
    match id {
        sys::SPA_VIDEO_COLOR_RANGE_16_235 => ColorRange::Limited,
        _ => ColorRange::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colorimetry(primaries: u32, transfer: u32) -> SpaColorimetry {
        SpaColorimetry {
            transfer,
            primaries,
            ..SpaColorimetry::default()
        }
    }

    #[test]
    fn a_surface_without_a_description_is_srgb() {
        let space = describe_color(&SpaColorimetry::default(), HdrLevels::default()).unwrap();

        assert_eq!(space.primaries, ColorPrimaries::Bt709);
        assert_eq!(space.transfer, TransferFunction::Srgb);
        assert_eq!(space.range, ColorRange::Full);
        assert_eq!(space.matrix, None);
        assert_eq!(space.hdr, HdrLevels::default());
    }

    #[test]
    fn bt2020_pq_is_reported_as_it_is() {
        let reported = SpaColorimetry {
            range: sys::SPA_VIDEO_COLOR_RANGE_0_255,
            matrix: sys::SPA_VIDEO_COLOR_MATRIX_BT2020,
            ..colorimetry(
                sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020,
                sys::SPA_VIDEO_TRANSFER_SMPTE2084,
            )
        };

        let space = describe_color(&reported, HdrLevels::default()).unwrap();

        assert_eq!(space.primaries, ColorPrimaries::Bt2020);
        assert_eq!(space.transfer, TransferFunction::Pq);
        assert_eq!(space.matrix, Some(MatrixCoefficients::Bt2020Ncl));
        assert_eq!(space.range, ColorRange::Full);
    }

    #[test]
    fn an_hdr_transfer_without_primaries_implies_bt2020() {
        for transfer in [
            sys::SPA_VIDEO_TRANSFER_SMPTE2084,
            sys::SPA_VIDEO_TRANSFER_ARIB_STD_B67,
        ] {
            let space = describe_color(&colorimetry(0, transfer), HdrLevels::default()).unwrap();

            assert_eq!(space.primaries, ColorPrimaries::Bt2020);
            assert!(space.transfer.is_hdr());
        }
    }

    #[test]
    fn hlg_and_linear_transfers_map_to_their_functions() {
        let transfer = |id| {
            describe_color(&colorimetry(0, id), HdrLevels::default())
                .unwrap()
                .transfer
        };

        assert_eq!(
            transfer(sys::SPA_VIDEO_TRANSFER_ARIB_STD_B67),
            TransferFunction::Hlg
        );
        assert_eq!(
            transfer(sys::SPA_VIDEO_TRANSFER_GAMMA10),
            TransferFunction::Linear
        );
        assert_eq!(
            transfer(sys::SPA_VIDEO_TRANSFER_GAMMA22),
            TransferFunction::Gamma22
        );
        assert_eq!(
            transfer(sys::SPA_VIDEO_TRANSFER_BT2020_10),
            TransferFunction::Bt709
        );
    }

    #[test]
    fn limited_range_and_bt601_matrix_are_carried() {
        let reported = SpaColorimetry {
            range: sys::SPA_VIDEO_COLOR_RANGE_16_235,
            matrix: sys::SPA_VIDEO_COLOR_MATRIX_BT601,
            ..colorimetry(
                sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTE170M,
                sys::SPA_VIDEO_TRANSFER_BT601,
            )
        };

        let space = describe_color(&reported, HdrLevels::default()).unwrap();

        assert_eq!(space.range, ColorRange::Limited);
        assert_eq!(space.matrix, Some(MatrixCoefficients::Bt601));
        assert_eq!(space.primaries, ColorPrimaries::Bt601_525);
    }

    #[test]
    fn rgb_and_exotic_matrices_leave_the_matrix_unset() {
        for id in [
            sys::SPA_VIDEO_COLOR_MATRIX_RGB,
            sys::SPA_VIDEO_COLOR_MATRIX_FCC,
            sys::SPA_VIDEO_COLOR_MATRIX_SMPTE240M,
        ] {
            let reported = SpaColorimetry {
                matrix: id,
                ..SpaColorimetry::default()
            };

            let space = describe_color(&reported, HdrLevels::default()).unwrap();

            assert_eq!(space.matrix, None);
        }
    }

    #[test]
    fn p3_primaries_map_to_the_named_sets() {
        let primaries = |id| {
            describe_color(
                &colorimetry(id, sys::SPA_VIDEO_TRANSFER_SRGB),
                HdrLevels::default(),
            )
            .unwrap()
            .primaries
        };

        assert_eq!(
            primaries(sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTEEG432),
            ColorPrimaries::DisplayP3
        );
        assert_eq!(
            primaries(sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTERP431),
            ColorPrimaries::DciP3
        );
    }

    #[test]
    fn adobe_rgb_primaries_become_custom_chromaticities() {
        let space = describe_color(
            &colorimetry(
                sys::SPA_VIDEO_COLOR_PRIMARIES_ADOBERGB,
                sys::SPA_VIDEO_TRANSFER_ADOBERGB,
            ),
            HdrLevels::default(),
        )
        .unwrap();

        let ColorPrimaries::Custom(chromaticities) = space.primaries else {
            panic!("adobe rgb primaries should be custom");
        };
        let (red_x, red_y) = chromaticities.red().xy();

        assert!((red_x - 0.64).abs() < 1e-4);
        assert!((red_y - 0.33).abs() < 1e-4);
        assert_eq!(space.transfer, TransferFunction::Gamma22);
    }

    #[test]
    fn every_custom_primary_set_is_a_valid_gamut() {
        for id in [
            sys::SPA_VIDEO_COLOR_PRIMARIES_ADOBERGB,
            sys::SPA_VIDEO_COLOR_PRIMARIES_BT470M,
            sys::SPA_VIDEO_COLOR_PRIMARIES_FILM,
            sys::SPA_VIDEO_COLOR_PRIMARIES_EBU3213,
        ] {
            let space = describe_color(
                &colorimetry(id, sys::SPA_VIDEO_TRANSFER_GAMMA22),
                HdrLevels::default(),
            );

            assert!(
                matches!(
                    space.map(|space| space.primaries),
                    Ok(ColorPrimaries::Custom(_))
                ),
                "primaries {id}"
            );
        }
    }

    #[test]
    fn unsupported_values_are_specific_errors() {
        let log = describe_color(
            &colorimetry(0, sys::SPA_VIDEO_TRANSFER_LOG100),
            HdrLevels::default(),
        );
        let xyz = describe_color(
            &colorimetry(sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTEST428, 0),
            HdrLevels::default(),
        );

        assert!(matches!(
            log,
            Err(CaptureError::TransferUnsupported {
                spa_transfer: sys::SPA_VIDEO_TRANSFER_LOG100
            })
        ));
        assert!(matches!(
            xyz,
            Err(CaptureError::PrimariesUnsupported {
                spa_primaries: sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTEST428
            })
        ));
    }

    #[test]
    fn the_configured_hdr_levels_travel_with_the_description() {
        let hdr = HdrLevels {
            peak_nits: 4000,
            ..HdrLevels::default()
        };

        let space = describe_color(
            &colorimetry(
                sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020,
                sys::SPA_VIDEO_TRANSFER_SMPTE2084,
            ),
            hdr,
        )
        .unwrap();

        assert_eq!(space.hdr, hdr);
    }
}

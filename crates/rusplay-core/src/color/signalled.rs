use crate::{
    color::{
        ColorPrimaries, ColorRange, ColorSpace, ContentLightLevel, MasteringDisplay,
        MatrixCoefficients, TransferFunction,
    },
    scale::ChromaSiting,
};

const AV1_CHROMA_POSITION_UNKNOWN: u32 = 0;
const AV1_CHROMA_POSITION_VERTICAL: u32 = 1;
const AV1_CHROMA_POSITION_COLOCATED: u32 = 2;
const VUI_CHROMA_LOCATION_TYPES: u32 = 6;
const VUI_CHROMA_LOCATION_LEFT: u32 = 0;
const VUI_CHROMA_LOCATION_CENTER: u32 = 1;

impl ChromaSiting {
    #[must_use]
    pub const fn vui_chroma_location(self) -> u32 {
        match self {
            Self::Left => VUI_CHROMA_LOCATION_LEFT,
            Self::Center => VUI_CHROMA_LOCATION_CENTER,
        }
    }

    #[must_use]
    pub const fn av1_chroma_position(self) -> u32 {
        match self {
            Self::Left => AV1_CHROMA_POSITION_VERTICAL,
            Self::Center => AV1_CHROMA_POSITION_UNKNOWN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SignalledColor {
    pub primaries: Option<ColorPrimaries>,
    pub transfer: Option<TransferFunction>,
    pub matrix: Option<MatrixCoefficients>,
    pub range: Option<ColorRange>,
    pub mastering: Option<MasteringDisplay>,
    pub content_light: Option<ContentLightLevel>,
    pub chroma_siting: Option<ChromaSiting>,
}

impl SignalledColor {
    #[must_use]
    pub fn with_h273_codes(self, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            primaries: ColorPrimaries::from_h273_code(primaries),
            transfer: TransferFunction::from_h273_code(transfer),
            matrix: MatrixCoefficients::from_h273_code(matrix),
            ..self
        }
    }

    #[must_use]
    pub const fn with_full_range(self, full: bool) -> Self {
        Self {
            range: Some(if full {
                ColorRange::Full
            } else {
                ColorRange::Limited
            }),
            ..self
        }
    }

    #[must_use]
    pub const fn with_vui_chroma_location(self, location_type: u32) -> Self {
        let chroma_siting = if location_type >= VUI_CHROMA_LOCATION_TYPES {
            None
        } else if location_type.is_multiple_of(2) {
            Some(ChromaSiting::Left)
        } else {
            Some(ChromaSiting::Center)
        };

        Self {
            chroma_siting,
            ..self
        }
    }

    #[must_use]
    pub const fn with_av1_chroma_position(self, position: u32) -> Self {
        let chroma_siting = match position {
            AV1_CHROMA_POSITION_VERTICAL | AV1_CHROMA_POSITION_COLOCATED => {
                Some(ChromaSiting::Left)
            }
            _ => None,
        };

        Self {
            chroma_siting,
            ..self
        }
    }

    pub fn over(&self, stream: ColorSpace) -> ColorSpace {
        let mut space = stream;

        if let Some(primaries) = self.primaries {
            space.primaries = primaries;
        }
        if let Some(transfer) = self.transfer {
            space.transfer = transfer;
        }
        if let Some(matrix) = self.matrix {
            space.matrix = Some(matrix);
        }
        if let Some(range) = self.range {
            space.range = range;
        }
        if let Some(mastering) = self.mastering {
            space.hdr.mastering = Some(mastering);
        }
        if let Some(content_light) = self.content_light {
            space.hdr.content_light = Some(content_light);
        }

        space
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Luminance;

    #[test]
    fn nothing_signalled_keeps_the_stream_description() {
        let stream = ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            ..ColorSpace::default()
        };

        assert_eq!(SignalledColor::default().over(stream), stream);
    }

    #[test]
    fn signalled_fields_win_over_the_stream_description() {
        let mastering = MasteringDisplay {
            max_nits: Luminance::from_whole_nits(4000).unwrap(),
            ..MasteringDisplay::default()
        };
        let signalled = SignalledColor {
            mastering: Some(mastering),
            ..SignalledColor::default()
        }
        .with_h273_codes(9, 16, 9)
        .with_full_range(false);

        let space = signalled.over(ColorSpace::default());

        assert_eq!(space.primaries, ColorPrimaries::Bt2020);
        assert_eq!(space.transfer, TransferFunction::Pq);
        assert_eq!(space.matrix, Some(MatrixCoefficients::Bt2020Ncl));
        assert_eq!(space.range, ColorRange::Limited);
        assert_eq!(space.hdr.mastering, Some(mastering));
        assert_eq!(space.hdr.content_light, None);
    }

    #[test]
    fn unspecified_codes_signal_nothing() {
        let signalled = SignalledColor::default().with_h273_codes(2, 2, 2);

        assert_eq!(signalled, SignalledColor::default());
    }

    #[test]
    fn vui_chroma_location_types_signal_the_horizontal_siting() {
        let siting = |location| {
            SignalledColor::default()
                .with_vui_chroma_location(location)
                .chroma_siting
        };

        for left in [0, 2, 4] {
            assert_eq!(siting(left), Some(ChromaSiting::Left));
        }
        for center in [1, 3, 5] {
            assert_eq!(siting(center), Some(ChromaSiting::Center));
        }
        assert_eq!(siting(6), None);
    }

    #[test]
    fn av1_chroma_positions_are_left_sited_when_known() {
        let siting = |position| {
            SignalledColor::default()
                .with_av1_chroma_position(position)
                .chroma_siting
        };

        assert_eq!(siting(0), None);
        assert_eq!(siting(1), Some(ChromaSiting::Left));
        assert_eq!(siting(2), Some(ChromaSiting::Left));
        assert_eq!(siting(3), None);
    }

    #[test]
    fn written_chroma_locations_read_back_as_the_same_siting() {
        for siting in [ChromaSiting::Left, ChromaSiting::Center] {
            let location =
                SignalledColor::default().with_vui_chroma_location(siting.vui_chroma_location());

            assert_eq!(location.chroma_siting, Some(siting));
        }
    }

    #[test]
    fn av1_writes_only_the_left_siting() {
        let position = |siting: ChromaSiting| {
            SignalledColor::default()
                .with_av1_chroma_position(siting.av1_chroma_position())
                .chroma_siting
        };

        assert_eq!(position(ChromaSiting::Left), Some(ChromaSiting::Left));
        assert_eq!(position(ChromaSiting::Center), None);
    }

    #[test]
    fn the_siting_does_not_change_the_colour_description() {
        let signalled = SignalledColor::default().with_vui_chroma_location(1);

        assert_eq!(signalled.over(ColorSpace::default()), ColorSpace::default());
    }

    #[test]
    fn bt709_transfer_aliases_signal_bt709() {
        for code in [6, 14, 15] {
            let signalled = SignalledColor::default().with_h273_codes(1, code, 1);

            assert_eq!(signalled.transfer, Some(TransferFunction::Bt709));
        }
    }
}

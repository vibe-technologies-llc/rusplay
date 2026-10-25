use crate::color::{
    ColorPrimaries, ColorRange, ColorSpace, ContentLightLevel, MasteringDisplay,
    MatrixCoefficients, TransferFunction,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SignalledColor {
    pub primaries: Option<ColorPrimaries>,
    pub transfer: Option<TransferFunction>,
    pub matrix: Option<MatrixCoefficients>,
    pub range: Option<ColorRange>,
    pub mastering: Option<MasteringDisplay>,
    pub content_light: Option<ContentLightLevel>,
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
}

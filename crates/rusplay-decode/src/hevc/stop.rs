#[derive(Debug, Clone, Copy)]
pub(super) enum Stop {
    ParserRejects,
    SubLayerCount,
    ScalingListReference,
    ShortTermSetSize,
    PalettePredictorSize,
    TileColumnCount,
    TileRowCount,
    TileColumnWidth,
    TileRowHeight,
    LongTermPictureCount,
    UnknownPictureParameterSet,
}

impl Stop {
    pub(super) fn reason(self) -> Option<&'static str> {
        match self {
            Self::ParserRejects => None,
            Self::SubLayerCount => Some("sub layer count above the maximum of seven"),
            Self::ScalingListReference => {
                Some("scaling list predicted from a matrix that does not precede it")
            }
            Self::ShortTermSetSize => {
                Some("short term reference picture set predicted from a set of 64 or more pictures")
            }
            Self::PalettePredictorSize => {
                Some("palette predictor initializer count above the palette predictor size")
            }
            Self::TileColumnCount => Some("more tile columns than the parser can hold"),
            Self::TileRowCount => Some("more tile rows than the parser can hold"),
            Self::TileColumnWidth => Some("tile column widths that do not fit the picture width"),
            Self::TileRowHeight => Some("tile row heights that do not fit the picture height"),
            Self::LongTermPictureCount => {
                Some("more long term reference pictures than the parser can hold")
            }
            Self::UnknownPictureParameterSet => {
                Some("slice header refers to a picture parameter set that was not accepted")
            }
        }
    }
}

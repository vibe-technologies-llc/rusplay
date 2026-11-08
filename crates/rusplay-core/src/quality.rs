mod xpsnr;

use std::fmt;

use thiserror::Error;

use crate::{BitDepth, ChromaSubsampling, FrameRate, PixelFormat, Resolution};

const PLANES: usize = 3;
const SECOND_ORDER_FROM_FPS: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QualityError {
    #[error("plane holds {actual} samples, {width}x{height} needs {needed}")]
    PlaneTooShort {
        width: usize,
        height: usize,
        needed: usize,
        actual: usize,
    },
    #[error("plane bytes hold {actual}, {width}x{height} at stride {stride} needs {needed}")]
    PlaneBytesTooShort {
        width: usize,
        height: usize,
        stride: usize,
        needed: usize,
        actual: usize,
    },
    #[error("chroma plane {plane} is {actual:?}, the {chroma} picture needs {expected:?}")]
    ChromaPlaneSize {
        plane: usize,
        chroma: ChromaSubsampling,
        expected: (usize, usize),
        actual: (usize, usize),
    },
    #[error("the reconstruction is {reconstruction:?}, the original {original:?}")]
    FormatMismatch {
        original: PixelFormat,
        reconstruction: PixelFormat,
    },
    #[error("the reconstruction is {reconstruction}, the original {original}")]
    SizeMismatch {
        original: Resolution,
        reconstruction: Resolution,
    },
    #[error("an earlier source luma plane is {actual:?}, the source {expected:?}")]
    HistorySizeMismatch {
        expected: (usize, usize),
        actual: (usize, usize),
    },
}

#[derive(Debug, Clone, Copy)]
pub struct PlaneBytes<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub bytes_per_sample: usize,
    pub shift: u32,
}

impl PlaneBytes<'_> {
    fn sample(&self, offset: usize) -> u16 {
        let raw = if self.bytes_per_sample == 2 {
            u16::from_le_bytes([self.data[offset], self.data[offset + 1]])
        } else {
            u16::from(self.data[offset])
        };

        raw >> self.shift
    }

    fn check(&self, width: usize, height: usize, per_pixel: usize) -> Result<(), QualityError> {
        let row = width * per_pixel * self.bytes_per_sample;
        let needed = if height == 0 {
            0
        } else {
            self.stride * (height - 1) + row
        };
        if self.data.len() < needed || (height > 1 && self.stride < row) {
            return Err(QualityError::PlaneBytesTooShort {
                width,
                height,
                stride: self.stride,
                needed,
                actual: self.data.len(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplePlane {
    width: usize,
    height: usize,
    samples: Vec<u16>,
}

impl SamplePlane {
    pub fn new(width: usize, height: usize, samples: Vec<u16>) -> Result<Self, QualityError> {
        let needed = width * height;
        if samples.len() < needed {
            return Err(QualityError::PlaneTooShort {
                width,
                height,
                needed,
                actual: samples.len(),
            });
        }

        let mut samples = samples;
        samples.truncate(needed);

        Ok(Self {
            width,
            height,
            samples,
        })
    }

    pub fn read(bytes: PlaneBytes<'_>, width: usize, height: usize) -> Result<Self, QualityError> {
        bytes.check(width, height, 1)?;

        let step = bytes.bytes_per_sample;
        let samples = (0..height)
            .flat_map(|y| (0..width).map(move |x| y * bytes.stride + x * step))
            .map(|offset| bytes.sample(offset))
            .collect();

        Ok(Self {
            width,
            height,
            samples,
        })
    }

    pub fn read_interleaved(
        bytes: PlaneBytes<'_>,
        width: usize,
        height: usize,
    ) -> Result<(Self, Self), QualityError> {
        bytes.check(width, height, 2)?;

        let step = bytes.bytes_per_sample;
        let mut first = Vec::with_capacity(width * height);
        let mut second = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let offset = y * bytes.stride + 2 * x * step;
                first.push(bytes.sample(offset));
                second.push(bytes.sample(offset + step));
            }
        }

        Ok((
            Self {
                width,
                height,
                samples: first,
            },
            Self {
                width,
                height,
                samples: second,
            },
        ))
    }

    pub const fn width(&self) -> usize {
        self.width
    }

    pub const fn height(&self) -> usize {
        self.height
    }

    pub fn samples(&self) -> &[u16] {
        &self.samples
    }

    const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn row(&self, y: usize) -> &[u16] {
        &self.samples[y * self.width..(y + 1) * self.width]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplePicture {
    format: PixelFormat,
    planes: [SamplePlane; PLANES],
}

impl SamplePicture {
    pub fn new(format: PixelFormat, planes: [SamplePlane; PLANES]) -> Result<Self, QualityError> {
        let [luma, cb, cr] = &planes;
        let expected = chroma_size(luma.size(), format.chroma);
        for (plane, chroma) in [(1, cb), (2, cr)] {
            if chroma.size() != expected {
                return Err(QualityError::ChromaPlaneSize {
                    plane,
                    chroma: format.chroma,
                    expected,
                    actual: chroma.size(),
                });
            }
        }

        Ok(Self { format, planes })
    }

    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn resolution(&self) -> Resolution {
        let luma = &self.planes[0];

        Resolution::new(
            u32::try_from(luma.width).unwrap_or(u32::MAX),
            u32::try_from(luma.height).unwrap_or(u32::MAX),
        )
    }

    pub fn luma(&self) -> &SamplePlane {
        &self.planes[0]
    }

    pub fn into_luma(self) -> SamplePlane {
        let [luma, _, _] = self.planes;
        luma
    }

    pub fn planes(&self) -> &[SamplePlane; PLANES] {
        &self.planes
    }
}

pub fn chroma_size((width, height): (usize, usize), chroma: ChromaSubsampling) -> (usize, usize) {
    match chroma {
        ChromaSubsampling::Yuv420 => (width.div_ceil(2), height.div_ceil(2)),
        ChromaSubsampling::Yuv422 => (width.div_ceil(2), height),
        ChromaSubsampling::Yuv444 => (width, height),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemporalOrder {
    First,
    Second,
}

impl TemporalOrder {
    pub fn for_rate(rate: FrameRate) -> Self {
        let whole_fps = rate
            .numerator
            .checked_div(rate.denominator)
            .unwrap_or_default();

        if whole_fps < SECOND_ORDER_FROM_FPS {
            Self::First
        } else {
            Self::Second
        }
    }

    pub const fn history_frames(self) -> usize {
        match self {
            Self::First => 1,
            Self::Second => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Decibels(f32);

impl Decibels {
    pub const IDENTICAL: Self = Self(f32::INFINITY);

    pub const fn new(value: f32) -> Self {
        Self(value)
    }

    fn from_error(error: f64, peak_squared: f64, samples: usize) -> Self {
        if error <= 0.0 || samples == 0 {
            return Self::IDENTICAL;
        }

        #[expect(clippy::cast_precision_loss)]
        let ratio = peak_squared * samples as f64 / error;
        #[expect(clippy::cast_possible_truncation)]
        let decibels = (10.0 * ratio.log10()) as f32;

        Self(decibels)
    }

    pub const fn value(self) -> f32 {
        self.0
    }

    pub const fn is_identical(self) -> bool {
        self.0.is_infinite() && self.0.is_sign_positive()
    }
}

impl fmt::Display for Decibels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_identical() {
            f.write_str("lossless")
        } else {
            write!(f, "{:.2} dB", self.0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneScores {
    pub luma: Decibels,
    pub cb: Decibels,
    pub cr: Decibels,
}

impl PlaneScores {
    pub const fn as_array(self) -> [Decibels; PLANES] {
        [self.luma, self.cb, self.cr]
    }

    const fn from_array([luma, cb, cr]: [Decibels; PLANES]) -> Self {
        Self { luma, cb, cr }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityScore {
    pub psnr: PlaneScores,
    pub psnr_overall: Decibels,
    pub xpsnr: PlaneScores,
}

pub fn measure(
    source: &SamplePicture,
    reconstruction: &SamplePicture,
    history: &[&SamplePlane],
    order: TemporalOrder,
) -> Result<QualityScore, QualityError> {
    if source.format != reconstruction.format {
        return Err(QualityError::FormatMismatch {
            original: source.format,
            reconstruction: reconstruction.format,
        });
    }
    if source.planes[0].size() != reconstruction.planes[0].size() {
        return Err(QualityError::SizeMismatch {
            original: source.resolution(),
            reconstruction: reconstruction.resolution(),
        });
    }
    for earlier in history {
        if earlier.size() != source.planes[0].size() {
            return Err(QualityError::HistorySizeMismatch {
                expected: source.planes[0].size(),
                actual: earlier.size(),
            });
        }
    }

    let peak_squared = peak_squared(source.format.depth);
    let errors: [f64; PLANES] = std::array::from_fn(|plane| {
        squared_error(&source.planes[plane], &reconstruction.planes[plane])
    });
    let sample_counts: [usize; PLANES] =
        std::array::from_fn(|plane| source.planes[plane].samples.len());

    let psnr = std::array::from_fn(|plane| {
        Decibels::from_error(errors[plane], peak_squared, sample_counts[plane])
    });
    let psnr_overall = Decibels::from_error(
        errors.iter().sum(),
        peak_squared,
        sample_counts.iter().sum(),
    );

    let weighted = xpsnr::weighted_errors(source, reconstruction, history, order);
    let xpsnr = std::array::from_fn(|plane| {
        Decibels::from_error(weighted[plane], peak_squared, sample_counts[plane])
    });

    Ok(QualityScore {
        psnr: PlaneScores::from_array(psnr),
        psnr_overall,
        xpsnr: PlaneScores::from_array(xpsnr),
    })
}

fn peak_squared(depth: BitDepth) -> f64 {
    let peak = f64::from((1_u32 << depth.bits()) - 1);

    peak * peak
}

fn squared_error(source: &SamplePlane, reconstruction: &SamplePlane) -> f64 {
    let total: u64 = source
        .samples
        .iter()
        .zip(&reconstruction.samples)
        .map(|(&a, &b)| {
            let difference = u64::from(a.abs_diff(b));
            difference * difference
        })
        .sum();

    #[expect(clippy::cast_precision_loss)]
    let total = total as f64;

    total
}

#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod tests;

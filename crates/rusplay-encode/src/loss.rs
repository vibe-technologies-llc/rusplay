use rusplay_core::{EncodedFrame, PixelFormat, Resolution};

use crate::{
    OutputIndex, Recovery, VideoEncoder,
    vaapi::tests::{Picture, encode_all, pattern_frames, pattern_planes, psnr},
};

pub(crate) const PSNR_FLOOR_DB: f64 = 34.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Loss {
    pub(crate) last_good: u64,
    pub(crate) resumed: u64,
    pub(crate) count: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Handling {
    Ignore,
    Recover { acknowledged: Option<u64> },
}

#[derive(Debug)]
pub(crate) struct LossRun {
    pub(crate) loss: Loss,
    pub(crate) packets: Vec<EncodedFrame>,
    pub(crate) recovery: Option<Recovery>,
}

impl LossRun {
    pub(crate) fn received(&self) -> Vec<u64> {
        (0..=self.loss.last_good)
            .chain(self.loss.resumed..self.loss.count)
            .collect()
    }

    pub(crate) fn kept(&self) -> Vec<EncodedFrame> {
        self.received()
            .into_iter()
            .map(|index| self.packets[usize::try_from(index).unwrap()].clone())
            .collect()
    }

    pub(crate) fn recovery_packets(&self) -> &[EncodedFrame] {
        &self.packets[usize::try_from(self.loss.resumed).unwrap()..]
    }

    pub(crate) fn referenced(&self) -> u64 {
        let Some(Recovery::Reference(OutputIndex(reference))) = self.recovery else {
            panic!("recovery fell back to a keyframe: {:?}", self.recovery);
        };
        assert!(reference <= self.loss.last_good);

        reference
    }

    pub(crate) fn check_decoded(
        &self,
        pictures: &[Picture],
        format: PixelFormat,
        resolution: Resolution,
    ) {
        let received = self.received();
        assert_eq!(pictures.len(), received.len());

        for (index, picture) in received.iter().zip(pictures) {
            assert_eq!(picture.resolution, resolution);
            assert_eq!(picture.format, format);

            let source = &pattern_planes(format, resolution, *index)[0];
            let quality = psnr(source, &picture.luma, format.depth);

            assert!(
                quality >= PSNR_FLOOR_DB,
                "frame {index} decodes at {quality:.1} dB after recovering from {:?}",
                self.recovery
            );
        }
    }
}

pub(crate) fn encode_with_loss(
    encoder: &mut dyn VideoEncoder,
    format: PixelFormat,
    resolution: Resolution,
    loss: Loss,
    handling: Handling,
) -> LossRun {
    let frames = pattern_frames(format, resolution, loss.count);
    let split = usize::try_from(loss.resumed).unwrap();

    let mut packets = encode_all(encoder, &frames[..split]);
    let recovery = match handling {
        Handling::Ignore => None,
        Handling::Recover { acknowledged } => {
            if let Some(output) = acknowledged {
                encoder.acknowledge(OutputIndex(output));
            }

            Some(encoder.recover(OutputIndex(loss.last_good)))
        }
    };
    packets.extend(encode_all(encoder, &frames[split..]));

    LossRun {
        loss,
        packets,
        recovery,
    }
}

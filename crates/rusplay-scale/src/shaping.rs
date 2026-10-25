use crate::Sigmoid;

const SIGMOIDIZE_INPUT: u32 = 1;
const UNSIGMOIDIZE_OUTPUT: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shaping {
    pub antiringing: f32,
    pub sigmoid: Option<Sigmoid>,
    pub sigmoidize_input: bool,
    pub unsigmoidize_output: bool,
}

impl Shaping {
    pub(crate) fn words(&self) -> [u32; 8] {
        let stage = |enabled: bool, flag: u32| {
            if enabled && self.sigmoid.is_some() {
                flag
            } else {
                0
            }
        };
        let stages = stage(self.sigmoidize_input, SIGMOIDIZE_INPUT)
            | stage(self.unsigmoidize_output, UNSIGMOIDIZE_OUTPUT);
        let sigmoid = self.sigmoid.unwrap_or_default();

        [
            self.antiringing.to_bits(),
            stages,
            sigmoid.center.to_bits(),
            sigmoid.slope.to_bits(),
            sigmoid.offset().to_bits(),
            sigmoid.scale().to_bits(),
            0,
            0,
        ]
    }

    #[cfg(test)]
    pub(crate) fn input(&self, texel: [f32; 4]) -> [f32; 4] {
        match self.sigmoid {
            Some(sigmoid) if self.sigmoidize_input => {
                let [red, green, blue, alpha] = texel;
                [
                    sigmoid.sigmoidize(red),
                    sigmoid.sigmoidize(green),
                    sigmoid.sigmoidize(blue),
                    alpha,
                ]
            }
            _ => texel,
        }
    }

    #[cfg(test)]
    pub(crate) fn output(&self, color: [f32; 4]) -> [f32; 4] {
        match self.sigmoid {
            Some(sigmoid) if self.unsigmoidize_output => {
                let [red, green, blue, alpha] = color;
                [
                    sigmoid.unsigmoidize(red),
                    sigmoid.unsigmoidize(green),
                    sigmoid.unsigmoidize(blue),
                    alpha,
                ]
            }
            _ => color,
        }
    }

    #[cfg(test)]
    pub(crate) fn antiring(&self, color: [f32; 4], low: [f32; 4], high: [f32; 4]) -> [f32; 4] {
        if self.antiringing <= 0.0 || low[0] > high[0] {
            return color;
        }

        std::array::from_fn(|channel| {
            let clamped = color[channel].clamp(low[channel], high[channel]);
            color[channel] + (clamped - color[channel]) * self.antiringing
        })
    }
}

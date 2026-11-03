use super::{bits::Bits, stop::Stop};

const SIZE_IDS: usize = 4;
const MATRIX_IDS: u32 = 6;
const LARGEST_SIZE_ID: usize = 3;
const LARGEST_SIZE_ID_MATRIX_STEP: u32 = 3;
const LONGEST_SCALING_LIST: usize = 64;
const LARGEST_SIZE_ID_WITHOUT_DC: usize = 1;

pub(super) fn scan_scaling_list_data(bits: &mut Bits) -> Result<(), Stop> {
    for size_id in 0..SIZE_IDS {
        let step = if size_id == LARGEST_SIZE_ID {
            LARGEST_SIZE_ID_MATRIX_STEP
        } else {
            1
        };

        for matrix_id in (0..MATRIX_IDS).step_by(step as usize) {
            if bits.bit()? {
                if size_id > LARGEST_SIZE_ID_WITHOUT_DC {
                    bits.signed()?;
                }
                let coefficients = LONGEST_SCALING_LIST.min(1 << (4 + (size_id << 1)));
                for _ in 0..coefficients {
                    bits.signed()?;
                }
            } else if u64::from(bits.unsigned()?) * u64::from(step) > u64::from(matrix_id) {
                return Err(Stop::ScalingListReference);
            }
        }
    }

    Ok(())
}

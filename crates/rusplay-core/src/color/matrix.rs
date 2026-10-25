use std::ops::Mul;

const BRADFORD: Matrix3 = Matrix3([
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
]);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix3(pub [[f64; 3]; 3]);

impl Matrix3 {
    pub const IDENTITY: Self = Self::diagonal([1.0; 3]);

    pub const fn diagonal(values: [f64; 3]) -> Self {
        Self([
            [values[0], 0.0, 0.0],
            [0.0, values[1], 0.0],
            [0.0, 0.0, values[2]],
        ])
    }

    pub const fn from_columns(columns: [[f64; 3]; 3]) -> Self {
        Self([
            [columns[0][0], columns[1][0], columns[2][0]],
            [columns[0][1], columns[1][1], columns[2][1]],
            [columns[0][2], columns[1][2], columns[2][2]],
        ])
    }

    pub const fn rows(&self) -> [[f64; 3]; 3] {
        self.0
    }

    pub fn apply(&self, vector: [f64; 3]) -> [f64; 3] {
        self.0
            .map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    }

    pub fn determinant(&self) -> f64 {
        let [a, b, c] = self.0;

        a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0])
    }

    pub fn inverse(&self) -> Option<Self> {
        let determinant = self.determinant();

        (determinant.is_finite() && determinant.abs() > f64::EPSILON)
            .then(|| self.adjugate_over(determinant))
    }

    pub(crate) fn inverse_of_invertible(&self) -> Self {
        self.adjugate_over(self.determinant())
    }

    fn adjugate_over(&self, determinant: f64) -> Self {
        let [a, b, c] = self.0;
        let cofactor = |value: f64| value / determinant;

        Self([
            [
                cofactor(b[1] * c[2] - b[2] * c[1]),
                cofactor(a[2] * c[1] - a[1] * c[2]),
                cofactor(a[1] * b[2] - a[2] * b[1]),
            ],
            [
                cofactor(b[2] * c[0] - b[0] * c[2]),
                cofactor(a[0] * c[2] - a[2] * c[0]),
                cofactor(a[2] * b[0] - a[0] * b[2]),
            ],
            [
                cofactor(b[0] * c[1] - b[1] * c[0]),
                cofactor(a[1] * c[0] - a[0] * c[1]),
                cofactor(a[0] * b[1] - a[1] * b[0]),
            ],
        ])
    }

    pub fn max_difference(&self, other: &Self) -> f64 {
        self.0
            .iter()
            .flatten()
            .zip(other.0.iter().flatten())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }
}

impl Mul for Matrix3 {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        Self(std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                (0..3)
                    .map(|inner| self.0[row][inner] * other.0[inner][column])
                    .sum()
            })
        }))
    }
}

pub fn bradford_adaptation(from_white: [f64; 3], to_white: [f64; 3]) -> Matrix3 {
    let from_cone = BRADFORD.apply(from_white);
    let to_cone = BRADFORD.apply(to_white);
    let gains = std::array::from_fn(|channel| to_cone[channel] / from_cone[channel]);

    BRADFORD.inverse_of_invertible() * Matrix3::diagonal(gains) * BRADFORD
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOLERANCE: f64 = 1e-12;

    #[test]
    fn inverse_times_matrix_is_identity() {
        let matrix = Matrix3([[2.0, 1.0, 0.5], [0.25, 3.0, 1.0], [1.0, 0.0, 4.0]]);

        let inverse = matrix.inverse().unwrap();

        assert!((matrix * inverse).max_difference(&Matrix3::IDENTITY) < TOLERANCE);
        assert!((inverse * matrix).max_difference(&Matrix3::IDENTITY) < TOLERANCE);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let matrix = Matrix3([[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 1.0, 1.0]]);

        assert_eq!(matrix.inverse(), None);
    }

    #[test]
    fn bradford_maps_source_white_onto_target_white() {
        let d65 = [0.950_47, 1.0, 1.088_83];
        let d50 = [0.964_22, 1.0, 0.825_21];

        let adapted = bradford_adaptation(d65, d50).apply(d65);

        for (got, want) in adapted.iter().zip(d50) {
            assert!((got - want).abs() < TOLERANCE);
        }
    }

    #[test]
    fn bradford_between_equal_whites_is_identity() {
        let d65 = [0.950_47, 1.0, 1.088_83];

        let adaptation = bradford_adaptation(d65, d65);

        assert!(adaptation.max_difference(&Matrix3::IDENTITY) < TOLERANCE);
    }
}

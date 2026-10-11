//! The DNG Camera Profile tone curve.
//!
//! `ProfileToneCurve` is a list of `(input, output)` pairs. The curve is the
//! piecewise-linear interpolation between them, held flat outside the first and
//! last input.

use std::fmt;

/// Errors from building a tone curve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToneCurveError {
    /// The flat pair list had an odd number of values.
    OddLength(usize),
    /// Fewer than two points.
    TooFewPoints(usize),
    /// Inputs were not strictly increasing.
    NotIncreasing { at: usize },
}

impl fmt::Display for ToneCurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToneCurveError::OddLength(n) => write!(f, "tone curve has odd length {n}"),
            ToneCurveError::TooFewPoints(n) => write!(f, "tone curve has {n} points, need >= 2"),
            ToneCurveError::NotIncreasing { at } => write!(f, "tone curve inputs not increasing at point {at}"),
        }
    }
}

impl std::error::Error for ToneCurveError {}

/// A piecewise-linear `input -> output` curve.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneCurve {
    points: Vec<[f64; 2]>,
}

impl ToneCurve {
    /// Build from a flat `[x0, y0, x1, y1, ...]` list (as stored in the tag).
    pub fn from_pairs(flat: &[f64]) -> Result<ToneCurve, ToneCurveError> {
        if !flat.len().is_multiple_of(2) {
            return Err(ToneCurveError::OddLength(flat.len()));
        }
        let points: Vec<[f64; 2]> = flat.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]).collect();
        ToneCurve::from_points(points)
    }

    /// The identity curve `y = x`.
    pub fn identity() -> ToneCurve {
        ToneCurve { points: vec![[0.0, 0.0], [1.0, 1.0]] }
    }

    /// Build from explicit `(input, output)` pairs.
    pub fn from_points(points: Vec<[f64; 2]>) -> Result<ToneCurve, ToneCurveError> {
        if points.len() < 2 {
            return Err(ToneCurveError::TooFewPoints(points.len()));
        }
        for i in 1..points.len() {
            if points[i][0] <= points[i - 1][0] {
                return Err(ToneCurveError::NotIncreasing { at: i });
            }
        }
        Ok(ToneCurve { points })
    }

    /// The `(input, output)` pairs.
    pub fn points(&self) -> &[[f64; 2]] {
        &self.points
    }

    /// Number of points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Evaluate the curve at `x`, clamping to the end values outside the range.
    pub fn eval(&self, x: f64) -> f64 {
        let p = &self.points;
        let Some(first) = p.first() else { return x.clamp(0.0, 1.0) };
        let Some(last) = p.last() else { return x.clamp(0.0, 1.0) };
        if x <= first[0] {
            return first[1];
        }
        if x >= last[0] {
            return last[1];
        }
        // First index whose input is greater than x.
        let i = p.partition_point(|q| q[0] <= x);
        let (x0, y0) = (p[i - 1][0], p[i - 1][1]);
        let (x1, y1) = (p[i][0], p[i][1]);
        let t = if x1 == x0 { 0.0 } else { (x - x0) / (x1 - x0) };
        y0 + t * (y1 - y0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_two_point() {
        let c = ToneCurve::from_pairs(&[0.0, 0.0, 1.0, 1.0]).unwrap();
        assert_eq!(c.eval(0.0), 0.0);
        assert_eq!(c.eval(0.5), 0.5);
        assert_eq!(c.eval(1.0), 1.0);
        assert_eq!(c.eval(-1.0), 0.0);
        assert_eq!(c.eval(2.0), 1.0);
    }

    #[test]
    fn interpolation_is_linear_between_knots() {
        let c = ToneCurve::from_pairs(&[0.0, 0.0, 1.0, 2.0]).unwrap();
        assert_eq!(c.eval(0.25), 0.5);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(ToneCurve::from_pairs(&[0.0, 1.0, 2.0]), Err(ToneCurveError::OddLength(3)));
        assert_eq!(ToneCurve::from_pairs(&[0.0, 1.0]), Err(ToneCurveError::TooFewPoints(1)));
    }
}

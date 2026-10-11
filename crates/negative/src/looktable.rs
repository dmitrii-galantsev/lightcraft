//! DNG hue/saturation tables: the `ProfileHueSatMap` (per-camera) and the
//! `ProfileLookTable` (universal shape).
//!
//! Both tags store the same structure — a 3-dimensional table of
//! `(hueShift, satScale, valScale)` triples indexed by `(hue, saturation,
//! value)`. The interpolation from the Adobe DNG SDK (`dng_hue_sat_map`,
//! `RefBaselineHueSatMap`) lives once, on [`HueSatTable`]; [`HueSatMap`] and
//! [`LookTable`] are thin wrappers that carry the table and its encoding.
//!
//! Storage order is value-major, hue next, saturation fastest:
//! `index = val * hueDivisions * satDivisions + hue * satDivisions + sat`.
//!
//! Encodings (`ProfileHueSatMapEncoding = 0xC7A3`,
//! `ProfileLookTableEncoding = 0xC7A4`): `0` linear, `1` sRGB gamma on the value
//! axis (used by the v2.3 LookTable). [`HueSatTable::sample`] is the pure
//! interpolation in the DNG internal hue range `[0, 6)` and does not encode.

use crate::color::{hsv_to_rgb, rgb_to_hsv, srgb_gamma_decode, srgb_gamma_encode};
use std::fmt;

/// Errors from building or decoding a hue/sat table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookTableError {
    /// The `Dims` tag was not three positive numbers.
    BadDims([u32; 3]),
    /// `le_bytes.len()` did not equal `dims product * 3 * 4`.
    LengthMismatch { expected: usize, got: usize },
    /// `le_bytes.len()` was not a multiple of 4.
    NotFloatAligned(usize),
}

impl fmt::Display for LookTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookTableError::BadDims(d) => write!(f, "bad hue/sat table dims {d:?}"),
            LookTableError::LengthMismatch { expected, got } => write!(f, "hue/sat table byte length {got} != dims*3*4 = {expected}"),
            LookTableError::NotFloatAligned(n) => write!(f, "hue/sat table byte length {n} is not a multiple of 4"),
        }
    }
}

impl std::error::Error for LookTableError {}

/// Value-axis encoding of a hue/sat table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HueSatEncoding {
    /// `encoding == 0` (or absent).
    #[default]
    Linear,
    /// `encoding == 1`.
    Srgb,
}

impl HueSatEncoding {
    /// Map the raw tag value to the encoding.
    pub fn from_tag(v: Option<u32>) -> HueSatEncoding {
        match v {
            Some(1) => HueSatEncoding::Srgb,
            _ => HueSatEncoding::Linear,
        }
    }
}

/// A `(hueShift, satScale, valScale)` modification at one table node.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HsbModify {
    /// Hue shift in degrees (DNG table units).
    pub hue_shift: f32,
    pub sat_scale: f32,
    pub val_scale: f32,
}

/// The shared decoded table: the one place the DNG interpolation lives.
#[derive(Debug, Clone, PartialEq)]
pub struct HueSatTable {
    dims: [u32; 3],
    /// Flat entries, `index = val*val_step + hue*hue_step + sat`.
    entries: Vec<[f32; 3]>,
}

impl HueSatTable {
    /// `[hue, saturation, value]` divisions.
    pub fn dims(&self) -> [u32; 3] {
        self.dims
    }

    /// Number of stored nodes: `hue * sat * val`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn hue_step(&self) -> usize {
        self.dims[1] as usize
    }

    fn val_step(&self) -> usize {
        self.dims[0] as usize * self.dims[1] as usize
    }

    /// The stored triple at grid node `(h, s, v)`, exactly as in the file
    /// (indices clamped into the table).
    pub fn node(&self, h: usize, s: usize, v: usize) -> HsbModify {
        let hd = (self.dims[0] as usize).max(1);
        let sd = (self.dims[1] as usize).max(1);
        let vd = (self.dims[2] as usize).max(1);
        let idx = v.min(vd - 1) * self.val_step() + h.min(hd - 1) * self.hue_step() + s.min(sd - 1);
        let e = self.entries.get(idx).copied().unwrap_or([0.0, 1.0, 1.0]);
        HsbModify { hue_shift: e[0], sat_scale: e[1], val_scale: e[2] }
    }

    /// Trilinear (2.5D for `val == 1`) interpolation, matching
    /// `RefBaselineHueSatMap` in the DNG SDK.
    ///
    /// - `hue6` is the DNG internal hue in `[0, 6)`.
    /// - `sat` and `val` are in `[0, 1]`; `val` is the *already encoded* value
    ///   axis coordinate when the table's encoding is sRGB.
    pub fn sample(&self, hue6: f32, sat: f32, val: f32) -> HsbModify {
        let hue6 = if hue6.is_finite() { hue6.rem_euclid(6.0) } else { 0.0 };
        let sat = if sat.is_finite() { sat.clamp(0.0, 1.0) } else { 0.0 };
        let val = if val.is_finite() { val.clamp(0.0, 1.0) } else { 0.0 };
        let hd = self.dims[0] as i32;
        let sd = self.dims[1] as i32;
        let vd = self.dims[2] as i32;
        if hd < 1 || sd < 1 || vd < 1 || self.entries.is_empty() {
            return HsbModify { hue_shift: 0.0, sat_scale: 1.0, val_scale: 1.0 };
        }
        let hue_step = sd;
        let val_step = hd * hue_step;
        let max_hue0 = hd - 1;
        let max_sat0 = (sd - 2).max(0);

        let get = |h: i32, s: i32, v: i32| -> [f32; 3] {
            self.entries.get((v * val_step + h * hue_step + s).max(0) as usize).copied().unwrap_or([0.0, 1.0, 1.0])
        };

        let h_scale = if hd < 2 { 0.0 } else { hd as f32 / 6.0 };
        let s_scale = (sd - 1) as f32;

        let (hue_shift, sat_scale, val_scale) = if vd < 2 {
            // 2.5D table: hue x saturation only.
            let h_scaled = hue6 * h_scale;
            let s_scaled = sat * s_scale;

            let mut h0 = (h_scaled as i32).clamp(0, max_hue0);
            let s0 = (s_scaled as i32).clamp(0, max_sat0);
            let mut h1 = h0 + 1;
            if h0 >= max_hue0 {
                h0 = max_hue0;
                h1 = 0;
            }

            let hf1 = h_scaled - h0 as f32;
            let hf0 = 1.0 - hf1;
            let sf1 = s_scaled - s0 as f32;
            let sf0 = 1.0 - sf1;

            let e00 = get(h0, s0, 0);
            let e01 = get(h1, s0, 0);
            let e00b = get(h0, s0 + 1, 0);
            let e01b = get(h1, s0 + 1, 0);

            let hs0 = hf0 * e00[0] + hf1 * e01[0];
            let ss0 = hf0 * e00[1] + hf1 * e01[1];
            let vs0 = hf0 * e00[2] + hf1 * e01[2];
            let hs1 = hf0 * e00b[0] + hf1 * e01b[0];
            let ss1 = hf0 * e00b[1] + hf1 * e01b[1];
            let vs1 = hf0 * e00b[2] + hf1 * e01b[2];

            (sf0 * hs0 + sf1 * hs1, sf0 * ss0 + sf1 * ss1, sf0 * vs0 + sf1 * vs1)
        } else {
            let max_val0 = (vd - 2).max(0);
            let v_scale = (vd - 1) as f32;

            let h_scaled = hue6 * h_scale;
            let s_scaled = sat * s_scale;
            let v_scaled = val * v_scale;

            let mut h0 = (h_scaled as i32).clamp(0, max_hue0);
            let s0 = (s_scaled as i32).clamp(0, max_sat0);
            let v0 = (v_scaled as i32).clamp(0, max_val0);
            let mut h1 = h0 + 1;
            if h0 >= max_hue0 {
                h0 = max_hue0;
                h1 = 0;
            }

            let hf1 = h_scaled - h0 as f32;
            let hf0 = 1.0 - hf1;
            let sf1 = s_scaled - s0 as f32;
            let sf0 = 1.0 - sf1;
            let vf1 = v_scaled - v0 as f32;
            let vf0 = 1.0 - vf1;

            let mut sat0 = [0.0f32; 3];
            let mut sat1 = [0.0f32; 3];
            for (k, out) in [0i32, 1].into_iter().zip([&mut sat0, &mut sat1]) {
                let s = s0 + k;
                let e00 = get(h0, s, v0);
                let e01 = get(h1, s, v0);
                let e10 = get(h0, s, v0 + 1);
                let e11 = get(h1, s, v0 + 1);
                for c in 0..3 {
                    out[c] = vf0 * (hf0 * e00[c] + hf1 * e01[c]) + vf1 * (hf0 * e10[c] + hf1 * e11[c]);
                }
            }
            (sf0 * sat0[0] + sf1 * sat1[0], sf0 * sat0[1] + sf1 * sat1[1], sf0 * sat0[2] + sf1 * sat1[2])
        };

        HsbModify { hue_shift, sat_scale, val_scale }
    }
}

/// A hue/sat table exactly as stored in the `.dcp`: dims plus raw little-endian
/// `f32` bytes. Used for both `ProfileHueSatMapData*` and `ProfileLookTableData`.
#[derive(Debug, Clone, PartialEq)]
pub struct HueSatTableData {
    /// `[hue, saturation, value]` divisions.
    pub dims: [u32; 3],
    /// Encoding tag value (`0xC7A3` for HueSatMap, `0xC7A4` for LookTable), if present.
    pub encoding: Option<u32>,
    /// `hue* sat* val * 3` little-endian `f32` values.
    pub le_bytes: Vec<u8>,
}

/// Back-compat alias: the raw LookTable payload is the same struct.
pub type LookTableData = HueSatTableData;

impl HueSatTableData {
    /// Number of `f32` values the table must hold: `dims product * 3`.
    pub fn value_count(&self) -> usize {
        (self.dims[0] as usize).saturating_mul(self.dims[1] as usize).saturating_mul(self.dims[2] as usize).saturating_mul(3)
    }

    /// Check that `dims` are positive and the payload is exactly the right size.
    pub fn validate(&self) -> Result<(), LookTableError> {
        if self.dims.contains(&0) {
            return Err(LookTableError::BadDims(self.dims));
        }
        let expected = self.value_count().saturating_mul(4);
        if self.le_bytes.len() != expected {
            return Err(LookTableError::LengthMismatch { expected, got: self.le_bytes.len() });
        }
        Ok(())
    }

    /// Decode the shared table (dims + floats), ignoring the encoding.
    pub fn decode_table(&self) -> Result<HueSatTable, LookTableError> {
        self.validate()?;
        if !self.le_bytes.len().is_multiple_of(4) {
            return Err(LookTableError::NotFloatAligned(self.le_bytes.len()));
        }
        let mut values = Vec::with_capacity(self.value_count());
        for c in self.le_bytes.as_chunks::<4>().0 {
            values.push(f32::from_le_bytes(*c));
        }
        let entries = values.as_chunks::<3>().0.to_vec();
        Ok(HueSatTable { dims: self.dims, entries })
    }

    /// Decode as a `ProfileLookTable`.
    pub fn decode(&self) -> Result<LookTable, LookTableError> {
        Ok(LookTable { table: self.decode_table()?, encoding: self.encoding })
    }

    /// Decode as a `ProfileHueSatMap`.
    pub fn decode_hue_sat_map(&self) -> Result<HueSatMap, LookTableError> {
        Ok(HueSatMap { table: self.decode_table()?, encoding: self.encoding })
    }
}

/// The `ProfileLookTable` (`0xC725`/`0xC726`).
#[derive(Debug, Clone, PartialEq)]
pub struct LookTable {
    table: HueSatTable,
    encoding: Option<u32>,
}

/// The `ProfileHueSatMap` (`0xC6F9`/`0xC6FA`/`0xC6FB`), the per-camera
/// calibration in the base and v2 generations.
#[derive(Debug, Clone, PartialEq)]
pub struct HueSatMap {
    table: HueSatTable,
    encoding: Option<u32>,
}

macro_rules! hue_sat_wrapper {
    ($ty:ident) => {
        impl $ty {
            /// The shared decoded table.
            pub fn table(&self) -> &HueSatTable {
                &self.table
            }

            /// `[hue, saturation, value]` divisions.
            pub fn dims(&self) -> [u32; 3] {
                self.table.dims
            }

            /// The raw encoding tag value, if present.
            pub fn encoding(&self) -> Option<u32> {
                self.encoding
            }

            /// Decoded value-axis encoding.
            pub fn encoding_kind(&self) -> HueSatEncoding {
                HueSatEncoding::from_tag(self.encoding)
            }

            /// Number of stored nodes.
            pub fn len(&self) -> usize {
                self.table.len()
            }

            pub fn is_empty(&self) -> bool {
                self.table.is_empty()
            }

            /// The stored triple at grid node `(h, s, v)`.
            pub fn node(&self, h: usize, s: usize, v: usize) -> HsbModify {
                self.table.node(h, s, v)
            }

            /// Pure interpolation (no value-axis encoding).
            pub fn sample(&self, hue6: f32, sat: f32, val: f32) -> HsbModify {
                self.table.sample(hue6, sat, val)
            }

            /// Full DNG modification of an RGB triple: RGB -> HSV, shift hue,
            /// scale saturation and value (with the value-axis encoding), back to
            /// RGB. Matches `RefBaselineHueSatMap`.
            pub fn apply_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
                apply_hue_sat(&self.table, self.encoding_kind(), rgb)
            }
        }
    };
}

hue_sat_wrapper!(LookTable);
hue_sat_wrapper!(HueSatMap);

/// The DNG hue/sat modification, shared by both wrappers.
fn apply_hue_sat(table: &HueSatTable, encoding: HueSatEncoding, rgb: [f32; 3]) -> [f32; 3] {
    let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
    if v <= 0.0 {
        return rgb;
    }

    let v_encoded = match encoding {
        HueSatEncoding::Linear => v,
        HueSatEncoding::Srgb => srgb_gamma_encode(v as f64) as f32,
    };

    let m = table.sample(h, s, v_encoded);

    let h2 = h + m.hue_shift * (6.0 / 360.0);
    let s2 = (s * m.sat_scale).clamp(0.0, 1.0);
    let ve2 = (v_encoded * m.val_scale).clamp(0.0, 1.0);
    let v2 = match encoding {
        HueSatEncoding::Linear => ve2,
        HueSatEncoding::Srgb => srgb_gamma_decode(ve2 as f64) as f32,
    };

    let (r, g, b) = hsv_to_rgb(h2, s2, v2);
    [r, g, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(dims: [u32; 3]) -> HueSatTable {
        let n = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        let entries = (0..n).map(|i| [i as f32, i as f32 + 0.5, i as f32 + 0.25]).collect();
        HueSatTable { dims, entries }
    }

    #[test]
    fn storage_order_is_val_hue_sat() {
        let t = table([3, 2, 2]);
        assert_eq!(t.node(0, 0, 0).hue_shift, 0.0);
        assert_eq!(t.node(1, 0, 0).hue_shift, 2.0);
        assert_eq!(t.node(0, 1, 0).hue_shift, 1.0);
        assert_eq!(t.node(0, 0, 1).hue_shift, 6.0);
    }

    #[test]
    fn identity_node_2p5d() {
        let t = table([36, 8, 1]);
        let n = t.node(5, 3, 0);
        let got = t.sample(5.0 * 6.0 / 36.0, 3.0 / 7.0, 0.0);
        for (a, b) in [(got.hue_shift, n.hue_shift), (got.sat_scale, n.sat_scale), (got.val_scale, n.val_scale)] {
            assert!((a - b).abs() <= 1e-4, "got {got:?} want {n:?}");
        }
    }

    #[test]
    fn neutral_input_is_unchanged_by_negative_saturation_node() {
        let dims = [36, 8, 1];
        let n = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        let t = HueSatTable { dims, entries: vec![[0.0, 1.0, 1.0]; n] };
        let out = apply_hue_sat(&t, HueSatEncoding::Linear, [0.3, 0.5, 0.7]);
        for i in 0..3 {
            assert!((out[i] - [0.3, 0.5, 0.7][i]).abs() < 1e-5, "{out:?}");
        }
    }

    #[test]
    fn rejects_wrong_length() {
        let data = LookTableData { dims: [36, 8, 1], encoding: None, le_bytes: vec![0u8; 10] };
        assert!(matches!(data.validate(), Err(LookTableError::LengthMismatch { .. })));
    }
}

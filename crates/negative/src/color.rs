//! Colour primitives for the negative conversion: matrices, DNG HSV, the
//! `CalibrationIlluminant` table, and the camera -> XYZ(D50) -> working-space
//! chain used to apply a DNG camera profile.
//!
//! The matrix pipeline follows the public Adobe DNG SDK
//! (`dng_color_spec::SetWhiteXY`, `dng_camera_profile::NormalizeForwardMatrix`,
//! `dng_temperature::Set_xy_coord`) and the documented standard illuminant
//! chromaticities. Nothing here is read from a plugin.
//!
//! Four-channel cameras are unsupported: a profile whose `ColorMatrix1` is 3x4
//! (RGBE sensors) needs a `ReductionMatrix` that ships nowhere, so
//! [`CameraColorSpec::from_dcp`] returns `None` for it rather than guess.

use crate::dcp::Dcp;
use std::fmt;

/// A row-major matrix of any shape, used for the `ColorMatrix` / `ForwardMatrix`
/// tags (3x3 for most cameras, 3x4 for four-channel-camera profiles).
#[derive(Debug, Clone, PartialEq)]
pub struct Matrix {
    rows: usize,
    cols: usize,
    data: Vec<f64>,
}

impl Matrix {
    /// `data` must hold `rows * cols` row-major values.
    pub fn new(rows: usize, cols: usize, data: Vec<f64>) -> Option<Matrix> {
        (data.len() == rows * cols).then_some(Matrix { rows, cols, data })
    }

    /// Build a 3-row matrix from a flat row-major list.
    pub fn from_rows3(data: Vec<f64>) -> Option<Matrix> {
        if !data.len().is_multiple_of(3) {
            return None;
        }
        Matrix::new(3, data.len() / 3, data)
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn data(&self) -> &[f64] {
        &self.data
    }

    pub fn get(&self, r: usize, c: usize) -> f64 {
        *self.data.get(r * self.cols + c).unwrap_or(&0.0)
    }

    /// View as a 3x3 matrix when it is one.
    pub fn as_mat3(&self) -> Option<Mat3> {
        if self.rows == 3 && self.cols == 3 { Mat3::from_row_major(&self.data) } else { None }
    }
}

/// A row-major 3x3 matrix of `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f64; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    /// From a row-major 9-element slice.
    pub fn from_row_major(v: &[f64]) -> Option<Mat3> {
        if v.len() != 9 {
            return None;
        }
        Some(Mat3([[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]))
    }

    /// Row-major 9-element array.
    pub fn row_major(&self) -> [f64; 9] {
        let m = self.0;
        [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
    }

    /// Matrix product `self * rhs`.
    pub fn mul(&self, rhs: &Mat3) -> Mat3 {
        let mut out = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                out[i][j] = (0..3).map(|k| self.0[i][k] * rhs.0[k][j]).sum();
            }
        }
        Mat3(out)
    }

    /// Matrix-vector product.
    pub fn mul_vec(&self, v: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0; 3];
        for i in 0..3 {
            out[i] = (0..3).map(|k| self.0[i][k] * v[k]).sum();
        }
        out
    }

    /// Scalar multiply.
    pub fn scale(&self, s: f64) -> Mat3 {
        let mut m = self.0;
        for row in &mut m {
            for x in row.iter_mut() {
                *x *= s;
            }
        }
        Mat3(m)
    }

    pub fn transpose(&self) -> Mat3 {
        let m = self.0;
        Mat3([[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]])
    }

    pub fn determinant(&self) -> f64 {
        let m = self.0;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// Inverse, or `None` when singular.
    pub fn inverse(&self) -> Option<Mat3> {
        let d = self.determinant();
        if d.abs() < 1e-15 {
            return None;
        }
        let m = self.0;
        let inv = [
            [(m[1][1] * m[2][2] - m[1][2] * m[2][1]) / d, (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / d, (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / d],
            [(m[1][2] * m[2][0] - m[1][0] * m[2][2]) / d, (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / d, (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / d],
            [(m[1][0] * m[2][1] - m[1][1] * m[2][0]) / d, (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / d, (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / d],
        ];
        Some(Mat3(inv))
    }
}

/// EXIF `LightSource` code, as used by `CalibrationIlluminant1/2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CalibrationIlluminant {
    Unknown = 0,
    Daylight = 1,
    Fluorescent = 2,
    Tungsten = 3,
    Flash = 4,
    FineWeather = 9,
    CloudyWeather = 10,
    Shade = 11,
    DaylightFluorescent = 12,
    DayWhiteFluorescent = 13,
    CoolWhiteFluorescent = 14,
    WhiteFluorescent = 15,
    WarmWhiteFluorescent = 16,
    StandardLightA = 17,
    StandardLightB = 18,
    StandardLightC = 19,
    D55 = 20,
    D65 = 21,
    D75 = 22,
    D50 = 23,
    IsoStudioTungsten = 24,
}

impl CalibrationIlluminant {
    /// Map an EXIF LightSource code to the enum.
    pub fn from_code(code: u16) -> Option<CalibrationIlluminant> {
        use CalibrationIlluminant::*;
        Some(match code {
            0 => Unknown,
            1 => Daylight,
            2 => Fluorescent,
            3 => Tungsten,
            4 => Flash,
            9 => FineWeather,
            10 => CloudyWeather,
            11 => Shade,
            12 => DaylightFluorescent,
            13 => DayWhiteFluorescent,
            14 => CoolWhiteFluorescent,
            15 => WhiteFluorescent,
            16 => WarmWhiteFluorescent,
            17 => StandardLightA,
            18 => StandardLightB,
            19 => StandardLightC,
            20 => D55,
            21 => D65,
            22 => D75,
            23 => D50,
            24 => IsoStudioTungsten,
            _ => return None,
        })
    }

    pub fn code(self) -> u16 {
        self as u16
    }

    /// Human name (EXIF / DNG spelling).
    pub fn name(self) -> &'static str {
        use CalibrationIlluminant::*;
        match self {
            Unknown => "Unknown",
            Daylight => "Daylight",
            Fluorescent => "Fluorescent",
            Tungsten => "Tungsten",
            Flash => "Flash",
            FineWeather => "Fine weather",
            CloudyWeather => "Cloudy weather",
            Shade => "Shade",
            DaylightFluorescent => "Daylight fluorescent",
            DayWhiteFluorescent => "Day white fluorescent",
            CoolWhiteFluorescent => "Cool white fluorescent",
            WhiteFluorescent => "White fluorescent",
            WarmWhiteFluorescent => "Warm white fluorescent",
            StandardLightA => "Standard Light A",
            StandardLightB => "Standard Light B",
            StandardLightC => "Standard Light C",
            D55 => "D55",
            D65 => "D65",
            D75 => "D75",
            D50 => "D50",
            IsoStudioTungsten => "ISO studio tungsten",
        }
    }

    /// Calibration temperature in Kelvin, per DNG's `IlluminantToTemperature`.
    /// `0.0` means "unknown".
    pub fn temperature(self) -> f64 {
        use CalibrationIlluminant::*;
        match self {
            StandardLightA | Tungsten => 2850.0,
            IsoStudioTungsten => 3200.0,
            D50 => 5000.0,
            D55 | Daylight | FineWeather | Flash | StandardLightB => 5500.0,
            D65 | StandardLightC | CloudyWeather => 6500.0,
            D75 | Shade => 7500.0,
            DaylightFluorescent => (5700.0 + 7100.0) * 0.5,
            DayWhiteFluorescent => (4600.0 + 5500.0) * 0.5,
            CoolWhiteFluorescent | Fluorescent => (3800.0 + 4500.0) * 0.5,
            WhiteFluorescent => (3250.0 + 3800.0) * 0.5,
            WarmWhiteFluorescent => (2600.0 + 3250.0) * 0.5,
            Unknown => 0.0,
        }
    }

    /// Standard CIE xy chromaticity, where the illuminant has a defined one.
    pub fn xy(self) -> Option<[f64; 2]> {
        use CalibrationIlluminant::*;
        Some(match self {
            StandardLightA | Tungsten => [0.4476, 0.4074],
            D50 => [0.3457, 0.3585],
            D55 => [0.3324, 0.3474],
            D65 => [0.3127, 0.3290],
            D75 => [0.2990, 0.3149],
            _ => return None,
        })
    }
}

impl fmt::Display for CalibrationIlluminant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// The D50 PCS white chromaticity.
pub const D50_XY: [f64; 2] = [0.3457, 0.3585];

/// `xy` -> `XYZ` with `Y = 1`, per DNG's `XYtoXYZ`.
pub fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    let x = xy[0].clamp(1e-6, 0.999_999);
    let mut y = xy[1].clamp(1e-6, 0.999_999);
    if x + y > 0.999_999 {
        y = 0.999_999 - x;
    }
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// `XYZ` -> `xy` chromaticity.
pub fn xyz_to_xy(xyz: [f64; 3]) -> [f64; 2] {
    let s = xyz[0] + xyz[1] + xyz[2];
    if s == 0.0 {
        return [0.0, 0.0];
    }
    [xyz[0] / s, xyz[1] / s]
}

/// XYZ(D50) white point with `Y = 1`.
pub fn d50_xyz() -> [f64; 3] {
    xy_to_xyz(D50_XY)
}

/// Linearised Bradford chromatic-adaptation matrix from `white1` to `white2`
/// (DNG's `MapWhiteMatrix`).
pub fn map_white_matrix(white1: [f64; 2], white2: [f64; 2]) -> Mat3 {
    const MB: Mat3 = Mat3([[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]]);
    let w1 = MB.mul_vec(xy_to_xyz(white1));
    let w2 = MB.mul_vec(xy_to_xyz(white2));
    let mut a = Mat3::IDENTITY;
    for i in 0..3 {
        let s1 = w1[i].max(0.0);
        let s2 = w2[i].max(0.0);
        a.0[i][i] = if s1 > 0.0 { (s2 / s1).clamp(0.1, 10.0) } else { 10.0 };
    }
    // MB is a constant, invertible matrix; identity is only a never-taken fallback.
    let Some(inv) = MB.inverse() else { return Mat3::IDENTITY };
    inv.mul(&a).mul(&MB)
}

/// Scale each row so it maps the camera neutral `(1,1,1)` to the PCS white
/// (DNG's `NormalizeForwardMatrix`).
pub fn normalize_forward_matrix(m: Mat3) -> Mat3 {
    let xyz = m.mul_vec([1.0, 1.0, 1.0]);
    let pc = d50_xyz();
    let mut scale = Mat3::IDENTITY;
    for i in 0..3 {
        if xyz[i] != 0.0 {
            scale.0[i][i] = pc[i] / xyz[i];
        }
    }
    scale.mul(&m)
}

/// sRGB gamma encode (DNG `GammaEncode_sRGB`), used by the v2.3 LookTable's
/// value axis (`ProfileLookTableEncoding = 1`).
pub fn srgb_gamma_encode(x: f64) -> f64 {
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Inverse of [`srgb_gamma_encode`].
pub fn srgb_gamma_decode(y: f64) -> f64 {
    if y <= 0.003_130_8 * 12.92 { y / 12.92 } else { ((y + 0.055) / 1.055).powf(2.4) }
}

/// XYZ(D50) -> linear ProPhoto RGB (the D50 working space NLP ships as
/// `ProPhoto.icc`). Lindbloom / Adobe ProPhoto primaries, D50 white.
pub fn xyz_d50_to_prophoto() -> Mat3 {
    Mat3([[1.345_943_3, -0.255_607_5, -0.051_111_8], [-0.544_598_9, 1.508_167_3, 0.020_535_1], [0.0, 0.0, 1.211_812_8]])
}

/// XYZ(D50) -> linear sRGB, Bradford-adapted to D50 (the ICC sRGB PCS).
pub fn xyz_d50_to_srgb() -> Mat3 {
    Mat3([[3.133_856_1, -1.616_866_7, -0.490_614_6], [-0.978_768_4, 1.916_141_5, 0.033_454_0], [0.071_945_3, -0.228_991_4, 1.405_242_7]])
}

/// `rgb -> hsv` in DNG's convention: hue in `[0, 6)`, sat/val in `[0, 1]`.
pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let v = r.max(g).max(b);
    let gap = v - r.min(g).min(b);
    if gap > 0.0 {
        let mut h = if r == v {
            let h = (g - b) / gap;
            if h < 0.0 { h + 6.0 } else { h }
        } else if g == v {
            2.0 + (b - r) / gap
        } else {
            4.0 + (r - g) / gap
        };
        if h < 0.0 {
            h += 6.0;
        }
        (h, gap / v, v)
    } else {
        (0.0, 0.0, v)
    }
}

/// `hsv -> rgb`, inverse of [`rgb_to_hsv`].
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    if s > 0.0 {
        let mut h = h;
        if h < 0.0 {
            h += 6.0;
        }
        if h >= 6.0 {
            h -= 6.0;
        }
        let i = h as i32;
        let f = h - i as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));
        match i {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            5 => (v, p, q),
            _ => (v, v, v),
        }
    } else {
        (v, v, v)
    }
}

/// Scale between distance in uv space and the "tint" parameter (DNG
/// `dng_temperature::kTintScale`).
const TINT_SCALE: f64 = -3000.0;

/// Robertson's isotemperature lines, from Wyszecki & Stiles, "Color Science",
/// 2nd ed., p. 228 (the table in Adobe's `dng_temperature.cpp`).
/// Each row is `(reciprocal-mega-kelvin, u, v, slope)`.
const TEMP_TABLE: [(f64, f64, f64, f64); 31] = [
    (0.0, 0.18006, 0.26352, -0.24341),
    (10.0, 0.18066, 0.26589, -0.25479),
    (20.0, 0.18133, 0.26846, -0.26876),
    (30.0, 0.18208, 0.27119, -0.28539),
    (40.0, 0.18293, 0.27407, -0.30470),
    (50.0, 0.18388, 0.27709, -0.32675),
    (60.0, 0.18494, 0.28021, -0.35156),
    (70.0, 0.18611, 0.28342, -0.37915),
    (80.0, 0.18740, 0.28668, -0.40955),
    (90.0, 0.18880, 0.28997, -0.44278),
    (100.0, 0.19032, 0.29326, -0.47888),
    (125.0, 0.19462, 0.30141, -0.58204),
    (150.0, 0.19962, 0.30921, -0.70471),
    (175.0, 0.20525, 0.31647, -0.84901),
    (200.0, 0.21142, 0.32312, -1.0182),
    (225.0, 0.21807, 0.32909, -1.2168),
    (250.0, 0.22511, 0.33439, -1.4512),
    (275.0, 0.23247, 0.33904, -1.7298),
    (300.0, 0.24010, 0.34308, -2.0637),
    (325.0, 0.24702, 0.34655, -2.4681),
    (350.0, 0.25591, 0.34951, -2.9641),
    (375.0, 0.26400, 0.35200, -3.5814),
    (400.0, 0.27218, 0.35407, -4.3633),
    (425.0, 0.28039, 0.35577, -5.3762),
    (450.0, 0.28863, 0.35714, -6.7262),
    (475.0, 0.29685, 0.35823, -8.5955),
    (500.0, 0.30505, 0.35907, -11.324),
    (525.0, 0.31320, 0.35968, -15.628),
    (550.0, 0.32129, 0.36011, -23.325),
    (575.0, 0.32931, 0.36038, -40.770),
    (600.0, 0.33724, 0.36051, -116.45),
];

/// Robertson's correlated colour temperature and tint for an `xy` chromaticity.
/// A faithful port of `dng_temperature::Set_xy_coord`. Returns
/// `(temperature_kelvin, tint)`.
pub fn temperature_and_tint_from_xy(xy: [f64; 2]) -> (f64, f64) {
    let denom = 1.5 - xy[0] + 6.0 * xy[1];
    if denom == 0.0 {
        return (0.0, 0.0);
    }
    let u = 2.0 * xy[0] / denom;
    let v = 3.0 * xy[1] / denom;

    let mut last_dt = 0.0;
    let mut last_du = 0.0;
    let mut last_dv = 0.0;

    for index in 1..=30usize {
        let dv = TEMP_TABLE[index].3;
        let len = (1.0 + dv * dv).sqrt();
        let du = 1.0 / len;
        let dv = dv / len;

        let uu = u - TEMP_TABLE[index].1;
        let vv = v - TEMP_TABLE[index].2;
        let dt = -uu * dv + vv * du;

        if dt <= 0.0 || index == 30 {
            let dt = if dt > 0.0 { 0.0 } else { -dt };
            let f = if index == 1 { 0.0 } else { dt / (last_dt + dt) };

            let temperature = 1.0e6 / (TEMP_TABLE[index - 1].0 * f + TEMP_TABLE[index].0 * (1.0 - f));

            let uu = u - (TEMP_TABLE[index - 1].1 * f + TEMP_TABLE[index].1 * (1.0 - f));
            let vv = v - (TEMP_TABLE[index - 1].2 * f + TEMP_TABLE[index].2 * (1.0 - f));

            let du = du * (1.0 - f) + last_du * f;
            let dv = dv * (1.0 - f) + last_dv * f;
            let len = (du * du + dv * dv).sqrt();
            let du = du / len;
            let dv = dv / len;

            let tint = (uu * du + vv * dv) * TINT_SCALE;
            return (temperature, tint);
        }

        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }
    // The loop always returns at index 30; only a non-finite input reaches here.
    (0.0, 0.0)
}

/// Correlated colour temperature (Kelvin) only.
pub fn temperature_from_xy(xy: [f64; 2]) -> f64 {
    temperature_and_tint_from_xy(xy).0
}

/// Inverse of [`temperature_and_tint_from_xy`]
/// (`dng_temperature::Get_xy_coord`).
pub fn xy_from_temperature(temperature: f64, tint: f64) -> [f64; 2] {
    if temperature == 0.0 {
        return D50_XY;
    }
    let r = 1.0e6 / temperature;
    let offset = tint * (1.0 / TINT_SCALE);

    for index in 0..=29usize {
        if r < TEMP_TABLE[index + 1].0 || index == 29 {
            let f = (TEMP_TABLE[index + 1].0 - r) / (TEMP_TABLE[index + 1].0 - TEMP_TABLE[index].0);
            let u = TEMP_TABLE[index].1 * f + TEMP_TABLE[index + 1].1 * (1.0 - f);
            let v = TEMP_TABLE[index].2 * f + TEMP_TABLE[index + 1].2 * (1.0 - f);

            let vv1 = TEMP_TABLE[index].3;
            let vv2 = TEMP_TABLE[index + 1].3;
            let len1 = (1.0 + vv1 * vv1).sqrt();
            let len2 = (1.0 + vv2 * vv2).sqrt();
            let uu1 = 1.0 / len1;
            let vv1 = vv1 / len1;
            let uu2 = 1.0 / len2;
            let vv2 = vv2 / len2;

            let uu3 = uu1 * f + uu2 * (1.0 - f);
            let vv3 = vv1 * f + vv2 * (1.0 - f);
            let len3 = (uu3 * uu3 + vv3 * vv3).sqrt();
            let uu3 = uu3 / len3;
            let vv3 = vv3 / len3;

            let u = u + uu3 * offset;
            let v = v + vv3 * offset;
            let d = u - 4.0 * v + 2.0;
            if d == 0.0 {
                return D50_XY;
            }
            return [1.5 * u / d, v / d];
        }
    }
    D50_XY
}

/// The camera-colour matrices of one profile, ready for white-point
/// interpolation.
#[derive(Debug, Clone)]
pub struct CameraColorSpec {
    pub color_matrix1: Mat3,
    pub color_matrix2: Mat3,
    pub forward_matrix1: Option<Mat3>,
    pub forward_matrix2: Option<Mat3>,
    pub temperature1: f64,
    pub temperature2: f64,
}

impl CameraColorSpec {
    /// Build from a parsed profile, deriving temperatures from the calibration
    /// illuminants and normalising any ForwardMatrix. Returns `None` when the
    /// profile has no `ColorMatrix1`, or is a four-channel camera whose
    /// `ColorMatrix1` is 3x4 rather than 3x3.
    pub fn from_dcp(dcp: &Dcp) -> Option<CameraColorSpec> {
        let mut cm1 = dcp.color_matrix1.as_ref()?.as_mat3()?;
        let mut cm2 = dcp.color_matrix2.as_ref().and_then(Matrix::as_mat3);
        let mut fm1 = dcp.forward_matrix1.as_ref().and_then(Matrix::as_mat3).map(normalize_forward_matrix);
        let mut fm2 = dcp.forward_matrix2.as_ref().and_then(Matrix::as_mat3).map(normalize_forward_matrix);

        let mut t1 = dcp.calibration_illuminant1.and_then(CalibrationIlluminant::from_code).map(CalibrationIlluminant::temperature).unwrap_or(0.0);
        let mut t2 = dcp.calibration_illuminant2.and_then(CalibrationIlluminant::from_code).map(CalibrationIlluminant::temperature).unwrap_or(0.0);

        if cm2.is_none() || t1 <= 0.0 || t2 <= 0.0 || t1 == t2 {
            t1 = 5000.0;
            t2 = 5000.0;
            cm2 = Some(cm1);
            fm2 = fm1;
        } else if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
            let c2 = cm2.take()?;
            cm2 = Some(cm1);
            cm1 = c2;
            std::mem::swap(&mut fm1, &mut fm2);
        }

        Some(CameraColorSpec {
            color_matrix1: cm1,
            color_matrix2: cm2.unwrap_or(cm1),
            forward_matrix1: fm1,
            forward_matrix2: fm2,
            temperature1: t1,
            temperature2: t2,
        })
    }

    /// Temperature fraction: 1.0 at `temperature1`, 0.0 at `temperature2`.
    pub fn temperature_to_g(&self, temperature: f64) -> f64 {
        if temperature <= self.temperature1 {
            1.0
        } else if temperature >= self.temperature2 {
            0.0
        } else {
            let inv = 1.0 / temperature;
            (inv - 1.0 / self.temperature2) / (1.0 / self.temperature1 - 1.0 / self.temperature2)
        }
    }

    /// Interpolated `(colorMatrix, forwardMatrix)` at blend factor `g`.
    pub fn blend(&self, g: f64) -> (Mat3, Option<Mat3>) {
        let lerp = |a: Mat3, b: Mat3| {
            let mut out = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    out[i][j] = g * a.0[i][j] + (1.0 - g) * b.0[i][j];
                }
            }
            Mat3(out)
        };
        let cm = if g >= 1.0 {
            self.color_matrix1
        } else if g <= 0.0 {
            self.color_matrix2
        } else {
            lerp(self.color_matrix1, self.color_matrix2)
        };
        let fm = match (self.forward_matrix1, self.forward_matrix2) {
            (Some(a), Some(b)) => Some(if g >= 1.0 {
                a
            } else if g <= 0.0 {
                b
            } else {
                lerp(a, b)
            }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        (cm, fm)
    }

    /// The camera neutral `(1,1,1)` in camera space for `white_xy` at blend `g`.
    pub fn camera_white(&self, white_xy: [f64; 2], g: f64) -> [f64; 3] {
        let (cm, _) = self.blend(g);
        let cw = cm.mul_vec(xy_to_xyz(white_xy));
        let scale = 1.0 / cw[0].max(cw[1]).max(cw[2]);
        let mut out = [0.0; 3];
        for i in 0..3 {
            out[i] = (scale * cw[i]).clamp(0.001, 1.0);
        }
        out
    }

    /// Camera -> XYZ(D50) for a target white, at an explicit blend factor.
    pub fn camera_to_xyzd50_at_g(&self, white_xy: [f64; 2], g: f64) -> Option<Mat3> {
        let (cm, fm) = self.blend(g);
        let cw = self.camera_white(white_xy, g);
        match fm {
            Some(fm) => {
                let mut inv = Mat3::IDENTITY;
                for i in 0..3 {
                    if cw[i] == 0.0 {
                        return None;
                    }
                    inv.0[i][i] = 1.0 / cw[i];
                }
                Some(fm.mul(&inv))
            }
            None => {
                let pcs_to_camera = cm.mul(&map_white_matrix(D50_XY, white_xy));
                let probe = pcs_to_camera.mul_vec(d50_xyz());
                let scale = probe[0].max(probe[1]).max(probe[2]);
                if scale == 0.0 {
                    return None;
                }
                pcs_to_camera.scale(1.0 / scale).inverse()
            }
        }
    }

    /// Camera -> XYZ(D50) for a target white. The blend factor is derived from
    /// the white's correlated colour temperature via
    /// [`temperature_from_xy`] (Robertson's estimator).
    pub fn camera_to_xyzd50(&self, white_xy: [f64; 2]) -> Option<Mat3> {
        let temperature = temperature_from_xy(white_xy);
        self.camera_to_xyzd50_at_temperature(white_xy, temperature)
    }

    /// Camera -> XYZ(D50) for a target white with an explicit temperature.
    pub fn camera_to_xyzd50_at_temperature(&self, white_xy: [f64; 2], white_temperature: f64) -> Option<Mat3> {
        let g = self.temperature_to_g(white_temperature);
        self.camera_to_xyzd50_at_g(white_xy, g)
    }

    /// The full chain: camera -> XYZ(D50) -> working space.
    pub fn camera_to_working(&self, white_xy: [f64; 2], xyz_d50_to_working: Mat3) -> Option<Mat3> {
        Some(xyz_d50_to_working.mul(&self.camera_to_xyzd50(white_xy)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn inverse_is_correct() {
        let m = Mat3([[1.0, 2.0, 3.0], [0.0, 1.0, 4.0], [5.0, 6.0, 0.0]]);
        let prod = m.mul(&m.inverse().unwrap());
        for i in 0..3 {
            for j in 0..3 {
                assert!(close(prod.0[i][j], if i == j { 1.0 } else { 0.0 }, 1e-12));
            }
        }
    }

    #[test]
    fn hsv_roundtrips() {
        for &(r, g, b) in &[(0.9f32, 0.2, 0.1), (0.1, 0.5, 0.9), (0.5, 0.5, 0.5), (0.0, 0.0, 0.0), (0.3, 0.8, 0.4)] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!(close(r2 as f64, r as f64, 1e-5), "r {r}->{r2}");
            assert!(close(g2 as f64, g as f64, 1e-5), "g {g}->{g2}");
            assert!(close(b2 as f64, b as f64, 1e-5), "b {b}->{b2}");
        }
    }

    #[test]
    fn srgb_gamma_roundtrips() {
        for i in 0..=100 {
            let x = i as f64 / 100.0;
            assert!(close(srgb_gamma_decode(srgb_gamma_encode(x)), x, 1e-9));
        }
    }

    #[test]
    fn temperature_estimator() {
        for &(t, tint) in &[(2850.0, 0.0), (5000.0, 0.0), (6500.0, 0.0), (5500.0, 10.0), (3200.0, -12.0)] {
            let xy = xy_from_temperature(t, tint);
            let (t2, tint2) = temperature_and_tint_from_xy(xy);
            assert!((t2 - t).abs() / t < 1e-3, "T {t} -> {t2}");
            assert!((tint2 - tint).abs() < 1.0, "tint {tint} -> {tint2}");
        }
    }

    #[test]
    fn camera_to_xyz_maps_camera_white_to_d50() {
        let fm = normalize_forward_matrix(Mat3([[0.7, 0.2, 0.1], [0.1, 0.8, 0.1], [0.05, 0.15, 0.8]]));
        let spec = CameraColorSpec {
            color_matrix1: Mat3([[1.3, -0.2, -0.1], [-0.1, 1.2, -0.1], [0.0, 0.1, 0.6]]),
            color_matrix2: Mat3([[1.4, -0.3, -0.1], [-0.1, 1.3, -0.2], [0.0, 0.1, 0.7]]),
            forward_matrix1: Some(fm),
            forward_matrix2: Some(fm),
            temperature1: 2850.0,
            temperature2: 6500.0,
        };
        let m = spec.camera_to_xyzd50_at_g(D50_XY, 1.0).unwrap();
        let got = m.mul_vec(spec.camera_white(D50_XY, 1.0));
        let want = d50_xyz();
        for i in 0..3 {
            assert!(close(got[i], want[i], 1e-9), "chan {i}: {got:?} vs {want:?}");
        }
    }
}

//! The classic negative-conversion pipeline.
//!
//! The **confirmed** stage is [`invert`]: a per-channel linear inverting window
//! `out = clamp01((white - in) / (white - dark))`, where `white` is the film base
//! measured from the frame border and `dark` is the negative's dark endpoint.
//! The window acts on the encoded (display-space) values — the evidence is the
//! first real conversion run, where the neutral grey wedge is reproduced to the
//! 8-bit floor by exactly this window (`nlp-re` notes 06, confirmed).
//!
//! The other stages ([`linearize`], [`neutralise`], [`apply_profile`],
//! [`apply_preset`], [`encode`]) follow the public DNG Camera Profile chain and
//! the preset vocabulary. They carry `// HYPOTHESIS` notes where the exact maths
//! is behavioural, and each is an entry on the `nlp-re` KNOWN-GAPS list. No
//! plugin data is read here: presets are modelled as parameters, profiles are
//! user-supplied `.dcp` files.

use crate::color::{CameraColorSpec, Mat3};
use crate::dcp::Dcp;
use crate::params::{ColorDensity, Params, PresetParams, ProcessOrder, ToneProfile};
use crate::tonecurve::ToneCurve;

// --------------------------------------------------------------------------- Image

/// An in-memory interleaved RGB float image, values nominally in `[0, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f32; 3]>,
}

impl Image {
    /// A black image.
    pub fn new(width: usize, height: usize) -> Image {
        Image { width, height, pixels: vec![[0.0; 3]; width.saturating_mul(height)] }
    }

    /// An image from an explicit row-major pixel buffer; `None` when the buffer
    /// does not match `width * height`.
    pub fn from_pixels(width: usize, height: usize, pixels: Vec<[f32; 3]>) -> Option<Image> {
        (pixels.len() == width.checked_mul(height)?).then_some(Image { width, height, pixels })
    }

    /// A constant image.
    pub fn fill(width: usize, height: usize, rgb: [f32; 3]) -> Image {
        Image { width, height, pixels: vec![rgb; width.saturating_mul(height)] }
    }

    /// A single-row synthetic image from a list of RGB triples.
    pub fn from_row(pixels: &[[f32; 3]]) -> Image {
        Image { width: pixels.len(), height: 1, pixels: pixels.to_vec() }
    }

    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// A copy with `f` applied to every pixel.
    pub fn mapped<F: Fn([f32; 3]) -> [f32; 3]>(&self, f: F) -> Image {
        Image { width: self.width, height: self.height, pixels: self.pixels.iter().map(|p| f(*p)).collect() }
    }

    /// Per-channel mean, clamped to `[0, 1]` first.
    pub fn mean(&self) -> [f32; 3] {
        self.mean_or([0.0; 3], |_| true)
    }

    /// Per-channel mean over pixels selected by `keep`.
    pub fn mean_or<F: Fn(usize) -> bool>(&self, fallback: [f32; 3], keep: F) -> [f32; 3] {
        let mut sum = [0.0f64; 3];
        let mut n = 0u64;
        for (i, p) in self.pixels.iter().enumerate() {
            if !keep(i) {
                continue;
            }
            for c in 0..3 {
                sum[c] += p[c].clamp(0.0, 1.0) as f64;
            }
            n += 1;
        }
        if n == 0 {
            return fallback;
        }
        [(sum[0] / n as f64) as f32, (sum[1] / n as f64) as f32, (sum[2] / n as f64) as f32]
    }

    /// The mean colour of the outer `border` fraction of the frame. A `border` of `0` still samples
    /// the outermost one-pixel ring (never the empty set).
    pub fn border_mean(&self, border: f32) -> [f32; 3] {
        self.mean_or([0.0; 3], |i| self.in_border(i, border))
    }

    /// Whether pixel `i` is in the outer `border` fraction (at least the one-pixel ring) of the
    /// frame.
    fn in_border(&self, i: usize, border: f32) -> bool {
        if self.width == 0 || self.height == 0 {
            return false;
        }
        let half_x = self.width.div_ceil(2);
        let half_y = self.height.div_ceil(2);
        let bx = ((self.width as f32 * border.clamp(0.0, 0.49)) as usize).clamp(1, half_x);
        let by = ((self.height as f32 * border.clamp(0.0, 0.49)) as usize).clamp(1, half_y);
        let x = i % self.width;
        let y = i / self.width;
        x < bx || x >= self.width.saturating_sub(bx) || y < by || y >= self.height.saturating_sub(by)
    }

    /// A per-channel high quantile `q` (`0..1`) of the outer `border` fraction's pixels: the
    /// statistic the film base is measured with (a high percentile is robust to a hot pixel or a
    /// speck of dust on the margin in a way a bare maximum is not). Empty region → `0`.
    pub fn border_quantile(&self, border: f32, q: f32) -> [f32; 3] {
        let mut vals: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (i, p) in self.pixels.iter().enumerate() {
            if !self.in_border(i, border) {
                continue;
            }
            for c in 0..3 {
                let v = p[c];
                vals[c].push(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
            }
        }
        let q = if q.is_finite() { q.clamp(0.0, 1.0) } else { 1.0 };
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let v = &mut vals[c];
            if v.is_empty() {
                continue;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let idx = (((v.len() - 1) as f32) * q).round() as usize;
            out[c] = v[idx.min(v.len() - 1)];
        }
        out
    }

    /// Per-channel minimum and maximum over clamped values.
    pub fn min_max(&self) -> ([f32; 3], [f32; 3]) {
        if self.pixels.is_empty() {
            return ([0.0; 3], [0.0; 3]);
        }
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for p in &self.pixels {
            for c in 0..3 {
                let v = p[c].clamp(0.0, 1.0);
                mn[c] = mn[c].min(v);
                mx[c] = mx[c].max(v);
            }
        }
        (mn, mx)
    }
}

// ---------------------------------------------------- ops — the pixel maths the conversion needs

/// Small, total, deterministic per-value helpers.
pub mod ops {
    /// Clamp to `[0, 1]`.
    pub fn clamp01(v: f32) -> f32 {
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
    }

    /// `-gamma g`: `pow(v, 1/g)`.
    pub fn gamma(v: f32, g: f32) -> f32 {
        if g == 0.0 { clamp01(v) } else { clamp01(v).powf(1.0 / g) }
    }

    /// `-negate`.
    pub fn negate(v: f32) -> f32 {
        1.0 - v
    }

    /// `-black-threshold t`: everything below `t` becomes black.
    pub fn black_threshold(v: f32, t: f32) -> f32 {
        if v < t { 0.0 } else { v }
    }

    /// `-white-threshold t`: everything above `t` becomes white.
    pub fn white_threshold(v: f32, t: f32) -> f32 {
        if v > t { 1.0 } else { v }
    }

    /// The confirmed per-channel inverting window: map `[black, white]` to
    /// `[1, 0]`, i.e. `(white - v) / (white - black)`. `white` is the measured
    /// film base and `black` the dark endpoint.
    ///
    /// A degenerate, inverted or unmeasured window (`white <= black`, or a base
    /// of `0`) is the identity rather than a garbage ramp: it must never invert
    /// a channel it cannot describe.
    pub fn invert_window(v: f32, white: f32, black: f32) -> f32 {
        let span = white - black;
        if !span.is_finite() || white <= 1e-4 || span <= 1e-6 { clamp01(v) } else { clamp01((white - v) / span) }
    }

    /// `-compose Multiply`: component-wise product.
    pub fn multiply(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
    }

    /// Scale saturation in HSV (the `-colorspace HSB` route).
    pub fn saturate(rgb: [f32; 3], amount: f32) -> [f32; 3] {
        let (h, s, v) = crate::color::rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
        let (r, g, b) = crate::color::hsv_to_rgb(h, (s * amount).clamp(0.0, 1.0), v);
        [r, g, b]
    }
}

// -------------------------------------------------------------------------- Stages

/// Stage 0: linearise the scan.
// HYPOTHESIS (nlp-re G1): the scan is stored with some display gamma; 2.2 is the
// classic recommendation. The confirmed window is a function of the *encoded*
// values, so the LightCraft default is `scan_gamma = 1.0` (already encoded) and
// this stage is then the identity.
pub fn linearize(img: &Image, scan_gamma: f32) -> Image {
    if (scan_gamma - 1.0).abs() < 1e-6 {
        return img.clone();
    }
    img.mapped(|p| [p[0].max(0.0).powf(scan_gamma), p[1].max(0.0).powf(scan_gamma), p[2].max(0.0).powf(scan_gamma)])
}

/// The per-channel high quantile of the border ring the film base is read from. The base is the
/// **bright** end of the negative (the unexposed margin), and the plugin stores it as a per-frame
/// per-channel **maximum**-like statistic (`nlp-re` notes 07/08); a *robust* high percentile is
/// used instead of a bare maximum so a hot pixel or a speck of dust on the margin cannot move it.
/// `0.95` is exact on the reference fixtures' uniform strips while leaving 5 % of the ring free.
pub const BASE_QUANTILE: f32 = 0.95;

/// Measure the film base from the border ring: the ring's per-channel high quantile
/// ([`BASE_QUANTILE`]). A mean here is the wrong statistic — on the reference fixtures it reads
/// 110/87/62 against the true 214/169/120.
///
/// The reference project names the film base `blackPointOrigin` (its names are from the negative's
/// side; the base is the *bright* end) and shows it behaves like a per-frame per-channel maximum:
/// it maps to output 0 in 259 of 332 two-point curves exactly, and the roll field `roll.rBlack` is
/// the max over the roll's frames of it (36 of 39 roll-channels exact, the rest within 3).
///
/// The dark endpoint is *not* measured here (see [`Params::dark`]).
pub fn measure_base(img: &Image, border: f32) -> [f32; 3] {
    img.border_quantile(border, BASE_QUANTILE)
}

/// Build a `Params` whose base is measured from `img`'s border and whose dark
/// endpoint is the documented placeholder.
pub fn params_from_measured_base(img: &Image, border: f32) -> Params {
    Params { base: measure_base(img, border), ..Params::default() }
}

/// Stage 1: invert the negative with the confirmed per-channel linear inverting
/// window. `params.base` is the measured film base and `params.dark` the dark
/// endpoint, both per channel.
pub fn invert(img: &Image, params: &Params) -> Image {
    img.mapped(|p| {
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            out[c] = ops::invert_window(p[c], params.base[c], params.dark[c]);
        }
        out
    })
}

/// The density exponent for `addDensity`.
// HYPOTHESIS (nlp-re G6): `addDensity` raises contrast with a fixed exponent.
const DENSITY_EXPONENT: f32 = 1.1;

/// Stage 2: neutralise — density, then the BlackClip/WhiteClip window.
// HYPOTHESIS (G6/G7/G8): `addDensity` = `x^1.1`, `neutralDensity` = identity;
// `blackClip`/`whiteClip` are per-channel `-black-threshold` / `-white-threshold`;
// the multipliers/clip act in the encoded (gamma) space.
pub fn neutralise(img: &Image, preset: &PresetParams) -> Image {
    let density_exp = match preset.color_density {
        ColorDensity::NeutralDensity => 1.0,
        ColorDensity::AddDensity => DENSITY_EXPONENT,
    };
    let black_clip = (-preset.black_clip / 100.0).clamp(0.0, 1.0);
    let white_clip = (1.0 + preset.white_clip / 100.0).clamp(0.0, 1.0);

    img.mapped(|p| {
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let v = ops::clamp01(p[c]);
            let v = if (density_exp - 1.0).abs() < 1e-6 { v } else { v.powf(density_exp) };
            let v = ops::black_threshold(v, black_clip);
            out[c] = ops::white_threshold(v, white_clip);
        }
        out
    })
}

/// Stage 3 (profile half): apply the camera profile — the colour matrix, the
/// HueSatMap, the LookTable, the BaselineExposureOffset and the tone curve.
///
/// Returns the image unchanged when the profile has no usable 3x3 colour matrix.
/// `amount` (0..1) blends the profiled result over the input.
pub fn apply_profile(img: &Image, dcp: &Dcp, white_xy: [f64; 2], working: crate::params::WorkingSpace, amount: f32) -> Image {
    let Some(spec) = CameraColorSpec::from_dcp(dcp) else {
        return img.clone();
    };
    let Some(m) = spec.camera_to_working(white_xy, working.xyz_d50_to_working()) else {
        return img.clone();
    };
    let m = mat3_to_f32(m);

    let hue_sat = dcp.hue_sat_map1.as_ref().and_then(|d| d.decode_hue_sat_map().ok());
    let look = dcp.look_table.as_ref().and_then(|d| d.decode().ok());
    let tone = dcp.profile_tone_curve.as_ref().and_then(|flat| ToneCurve::from_pairs(flat).ok());
    let exposure = dcp.baseline_exposure_offset.map(|e| (2.0f64.powf(e)) as f32);
    let a = if amount.is_finite() { amount.clamp(0.0, 1.0) } else { 1.0 };

    img.mapped(|p| {
        let mut rgb = [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
        ];
        if let Some(hs) = &hue_sat {
            rgb = hs.apply_rgb(rgb);
        }
        if let Some(lt) = &look {
            rgb = lt.apply_rgb(rgb);
        }
        if let Some(e) = exposure {
            for c in &mut rgb {
                *c *= e;
            }
        }
        if let Some(tc) = &tone {
            for c in &mut rgb {
                *c = tc.eval(*c as f64) as f32;
            }
        }
        if a < 1.0 {
            for c in 0..3 {
                rgb[c] = p[c] + (rgb[c] - p[c]) * a;
            }
        }
        rgb
    })
}

/// Build the classic conversion's tone curve for a `ToneProfile`, with the
/// Lights/Darks/Whites/Blacks sliders as knot offsets.
// HYPOTHESIS (G14/G15): the whole shape and the knot offsets.
pub fn tone_curve_for(preset: &PresetParams) -> ToneCurve {
    let shape = |x: f32| match preset.tone_profile {
        ToneProfile::Linear => x,
        ToneProfile::LinearGamma => x.powf(1.0 / 1.1),
        ToneProfile::Logarithmic => (1.0 + 9.0 * x).ln() / 10f32.ln(),
        ToneProfile::Standard => x * x * (3.0 - 2.0 * x), // smoothstep
    };
    let blacks = preset.blacks / 100.0 * 0.25;
    let darks = preset.darks / 100.0 * 0.25;
    let lights = preset.lights / 100.0 * 0.25;
    let whites = preset.whites / 100.0 * 0.25;
    let xs = [0.0f32, 0.25, 0.5, 0.75, 1.0];
    let offsets = [blacks, darks, 0.0, lights, whites];
    let points: Vec<[f64; 2]> = xs.iter().zip(offsets).map(|(&x, o)| [x as f64, (shape(x) + o).clamp(0.0, 1.0) as f64]).collect();
    // `xs` is strictly increasing by construction, so this only falls back for a
    // non-finite slider value.
    ToneCurve::from_points(points).unwrap_or_else(|_| ToneCurve::identity())
}

/// Region weights `(shadows, mids, highs)` for a luminance.
// HYPOTHESIS (G16): triangular membership widened by `ShadowsRange`/`HighsRange`.
fn region_weights(lum: f32, shadows_range: f32, highs_range: f32) -> [f32; 3] {
    let sr = (shadows_range / 10.0).clamp(0.1, 1.0);
    let hr = (highs_range / 10.0).clamp(0.1, 1.0);
    let shadow = (1.0 - lum / sr).clamp(0.0, 1.0);
    let high = ((lum - (1.0 - hr)) / hr).clamp(0.0, 1.0);
    let mid = (1.0 - (lum - 0.5).abs() * 2.0).clamp(0.0, 1.0);
    [shadow, mid, high]
}

/// A named paper/film look as a per-channel gain.
// HYPOTHESIS (G17): `LUT` selects a fixed 3-channel gain; the numbers are
// placeholders pending a fit.
fn lut_gains(lut: crate::params::Lut) -> [f32; 3] {
    use crate::params::Lut::*;
    match lut {
        Crystal => [1.02, 1.00, 1.03],
        Frontier => [1.03, 1.00, 1.00],
        Natural => [1.00, 1.00, 1.00],
        Pakon => [0.99, 1.01, 1.02],
    }
}

/// Stage 4: apply the preset — region colour, saturation, LUT, tone curve,
/// fade/glow.
// HYPOTHESIS (G18/G19): every numeric transfer here.
pub fn apply_preset(img: &Image, preset: &PresetParams) -> Image {
    let curve = tone_curve_for(preset);
    let gains = lut_gains(preset.lut);
    let sat = 1.0 + preset.saturation / 100.0;
    let fade = preset.lab_fade / 100.0;
    let glow = preset.lab_glow / 100.0;
    let sr = preset.shadows_range;
    let hr = preset.highs_range;
    let rc = preset.red_cyan;
    let gm = preset.green_magenta;
    let by = preset.blue_yellow;
    let colour_first = matches!(preset.process_order, ProcessOrder::ColorFirst);

    img.mapped(|p| {
        let mut rgb = p;
        let apply_colour = |rgb: [f32; 3]| -> [f32; 3] {
            let lum = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            let w = region_weights(lum, sr, hr);
            let mut out = rgb;
            for c in 0..3 {
                let axis = match c {
                    0 => rc,
                    1 => gm,
                    _ => by,
                };
                let delta = (axis[0] * w[0] + axis[1] * w[1] + axis[2] * w[2]) / 100.0;
                out[c] = ops::clamp01(out[c] + delta);
            }
            let mut out = ops::saturate(out, sat);
            if gains != [1.0; 3] {
                out = ops::multiply(out, gains);
            }
            out
        };
        let apply_tone = |rgb: [f32; 3]| -> [f32; 3] {
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                out[c] = curve.eval(rgb[c] as f64) as f32;
            }
            out
        };
        if colour_first {
            rgb = apply_colour(rgb);
            rgb = apply_tone(rgb);
        } else {
            rgb = apply_tone(rgb);
            rgb = apply_colour(rgb);
        }
        for c in 0..3 {
            rgb[c] = ops::clamp01(rgb[c] + fade * 0.05 + glow * rgb[c] * rgb[c] * 0.1);
        }
        rgb
    })
}

/// Stage 6: encode into the output gamma. `output_gamma = 1.0` is the identity
/// (the result stays in the encoded space the invert acted in).
pub fn encode(img: &Image, output_gamma: f32) -> Image {
    if (output_gamma - 1.0).abs() < 1e-6 {
        return img.clone();
    }
    img.mapped(|p| [p[0].max(0.0).powf(1.0 / output_gamma), p[1].max(0.0).powf(1.0 / output_gamma), p[2].max(0.0).powf(1.0 / output_gamma)])
}

// ------------------------------------------------------------------------- convert

/// Run the whole classic conversion end to end. Deterministic.
pub fn convert(img: &Image, params: &Params) -> Image {
    let linear = linearize(img, params.scan_gamma);
    let inverted = invert(&linear, params);
    let neutral = neutralise(&inverted, &params.preset);
    let profiled = match &params.profile {
        Some(dcp) => apply_profile(&neutral, dcp, params.white_xy, params.working_space, params.profile_amount),
        None => neutral,
    };
    let looked = apply_preset(&profiled, &params.preset);
    encode(&looked, params.output_gamma)
}

fn mat3_to_f32(m: Mat3) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = m.0[i][j] as f32;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::PresetParams;

    fn base_state(base: [f32; 3]) -> Params {
        Params { base, scan_gamma: 1.0, output_gamma: 1.0, ..Params::default() }
    }

    #[test]
    fn invert_window_reproduces_the_grey_wedge() {
        // Measured (8-bit) input -> output from the reference run: base
        // 214/169/120, dark 7.17/1.56/-0.89.
        let base = [214.0 / 255.0, 169.0 / 255.0, 120.0 / 255.0];
        let mut p = base_state(base);
        p.dark = [7.17 / 255.0, 1.56 / 255.0, -0.89 / 255.0];
        let cases: [([f32; 3], [f32; 3]); 2] = [([97.0, 77.0, 54.0], [144.0, 140.0, 137.0]), ([43.0, 34.0, 24.0], [210.0, 206.0, 204.0])];
        for (inp8, want8) in cases {
            let inp = Image::from_row(&[[inp8[0] / 255.0, inp8[1] / 255.0, inp8[2] / 255.0]]);
            let out = invert(&inp, &p);
            for c in 0..3 {
                let got = out.pixels[0][c] * 255.0;
                assert!((got - want8[c]).abs() < 3.0, "ch{c}: got {got}, want {}", want8[c]);
            }
        }
    }

    #[test]
    fn measure_base_sees_the_border() {
        let mut px = vec![[0.1f32, 0.1, 0.1]; 16];
        for y in 0..4 {
            for x in 0..4 {
                if x == 0 || y == 0 || x == 3 || y == 3 {
                    px[y * 4 + x] = [0.8, 0.7, 0.6];
                }
            }
        }
        let img = Image::from_pixels(4, 4, px).unwrap();
        let base = measure_base(&img, 0.25);
        assert!((base[0] - 0.8).abs() < 0.01 && (base[1] - 0.7).abs() < 0.01 && (base[2] - 0.6).abs() < 0.01, "{base:?}");
    }

    #[test]
    fn degenerate_window_is_the_identity() {
        // An unmeasured base (0) with a positive dark endpoint is an inverted window; it must pass
        // the channel through rather than turn it into garbage.
        let img = Image::from_row(&[[0.25, 0.5, 0.75]]);
        let p = Params { base: [0.0; 3], dark: [7.17 / 255.0, 1.56 / 255.0, -0.89 / 255.0], ..Params::default() };
        let out = invert(&img, &p);
        for c in 0..3 {
            assert!((out.pixels[0][c] - img.pixels[0][c]).abs() < 1e-6, "{:?}", out.pixels[0]);
        }
    }

    #[test]
    fn zero_border_samples_the_outer_ring() {
        // A `border` of 0 must not sample the empty set.
        let img = Image::fill(3, 3, [0.5, 0.5, 0.5]);
        assert_eq!(measure_base(&img, 0.0), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn measure_base_reads_the_bright_end_not_the_mean() {
        // A uniform base strip on the left (the brightest), the rest mid: the ring's mean is
        // dragged down, the high quantile reads the base.
        let (w, h) = (20usize, 20usize);
        let base = [0.9f32, 0.8, 0.7];
        let mut px = vec![[0.3f32, 0.3, 0.3]; w * h];
        for y in 0..h {
            for x in 0..4 {
                px[y * w + x] = base;
            }
        }
        let img = Image::from_pixels(w, h, px).unwrap();
        assert_eq!(measure_base(&img, 0.0), base);
        let mean = img.border_mean(0.0);
        assert!(mean[0] < 0.8, "the mean is dragged down: {mean:?}");
    }

    #[test]
    fn measure_base_ignores_a_hot_pixel() {
        // One overexposed pixel (dust / a light leak) on the margin: the high quantile is unmoved,
        // a bare maximum is not.
        let (w, h) = (20usize, 20usize);
        let base = [0.9f32, 0.8, 0.7];
        let mut px = vec![[0.3f32, 0.3, 0.3]; w * h];
        for y in 0..h {
            for x in 0..4 {
                px[y * w + x] = base;
            }
        }
        px[0] = [1.0, 1.0, 1.0];
        let img = Image::from_pixels(w, h, px).unwrap();
        assert_eq!(measure_base(&img, 0.0), base);
        assert_eq!(img.border_quantile(0.0, 1.0), [1.0, 1.0, 1.0], "a bare max would move");
    }

    #[test]
    fn neutral_conversion_is_the_window() {
        // A neutral preset with gamma 1 in/out reduces to the per-channel window.
        let p = base_state([0.84, 0.66, 0.47]);
        let img = Image::from_row(&[[0.5, 0.5, 0.5]]);
        let out = convert(&img, &p);
        // Base maps to 0, black (0.0) maps to 1.
        let at_base = convert(&Image::from_row(&[[0.84, 0.66, 0.47]]), &p);
        assert!(at_base.pixels[0].iter().all(|v| v.abs() < 1e-6), "{:?}", at_base.pixels[0]);
        assert!(out.pixels[0].iter().all(|v| v.is_finite()));
    }

    #[test]
    fn linearize_then_encode_roundtrips() {
        let px: Vec<[f32; 3]> = vec![[0.1, 0.5, 0.9], [0.3, 0.4, 0.6]];
        let img = Image::from_row(&px);
        let back = encode(&linearize(&img, 2.2), 2.2);
        for (a, b) in back.pixels.iter().zip(px.iter()) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn add_density_raises_contrast() {
        let mut p = base_state([1.0; 3]);
        p.preset.color_density = ColorDensity::AddDensity;
        let img = Image::from_row(&[[0.25, 0.25, 0.25]]);
        let out = neutralise(&img, &p.preset);
        assert!(out.pixels[0][0] < 0.25, "{:?}", out.pixels[0]);
    }

    #[test]
    fn convert_is_deterministic() {
        let p = base_state([0.8, 0.7, 0.6]);
        let img = Image::from_row(&[[0.2, 0.3, 0.4], [0.7, 0.6, 0.5]]);
        let a = convert(&img, &p);
        let b = convert(&img, &p);
        assert_eq!(a, b);
        assert!(a.pixels.iter().all(|q| q.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn empty_preset_is_identity_after_invert_is_neutral() {
        // A base of 1 and dark of 0 is the negate; a "full range" window.
        let p = base_state([1.0; 3]);
        let img = Image::from_row(&[[0.25, 0.25, 0.25]]);
        let out = invert(&img, &p);
        assert!((out.pixels[0][0] - 0.75).abs() < 1e-6, "{:?}", out.pixels[0]);
    }

    #[test]
    fn presets_are_defaults_and_clone() {
        let p = PresetParams::default();
        assert_eq!(p.tone_profile, ToneProfile::Linear);
        assert_eq!(p.saturation, 0.0);
    }
}

//! Scene-linear preparation: white balance + exposure, and the spatial planes the per-pixel stage
//! needs (edge-aware base layer for highlights/shadows, clarity/texture bands, dehaze veil).

use lightcraft_color::cct::{temp_tint_to_xy, wb_matrix, xy_to_temp_tint};
use lightcraft_color::{REC2020, luminance_2020};
use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_raster::blur::gaussian;
use std::sync::Arc;

use lightcraft_raster::{Plane, Rgb32f, par_join};

use crate::geometry::Frame;
use crate::{Prepared, Quality, SourceInfo, for_rows, masks, timed};

/// White balance (relative to the source's as-shot white) and exposure, in place.
pub fn scene_linear_pre(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 2f32.powf(s.light.exposure as f32));
}

/// The film-base white balance of a negative: the per-channel gains that neutralize the measured
/// film base, proportional to `1/base` in ratio (the plugin's own per-roll state stores exactly
/// such gains — `greenMultiplier`/`blueMultiplier`, "red is the reference channel") and
/// **luminance-preserving** in scale: the neutralized base lands at the base's own Rec.2020
/// luminance, because a white balance must not change exposure — the invariant [`wb_matrix`]
/// upholds for every other source.
///
/// This is the confirmed plugin operation, not a colour temperature. The plugin's recorded
/// per-photo `Temperature`/`Tint` is how that neutralization is serialized into a Lightroom
/// control (2000–2650 K on real colour scans), not a scene illuminant: no Kelvin value reproduces
/// the recorded neutral bases through a colour-temperature model, and adapting the developed
/// pixels that far drives channels out of gamut (the blue channel of the film margin pegs at 255).
///
/// `base_enc` is the film base in encoded sRGB (what `develop.negativeMeasure` stores).
/// `None` for a degenerate or unmeasured base.
pub fn negative_base_gains(base_enc: [f64; 3]) -> Option<[f64; 3]> {
    let b = lightcraft_negative::srgb_encoded_to_linear_rec2020(base_enc.map(|v| v as f32)).map(f64::from);
    let y = 0.2627 * b[0] + 0.6780 * b[1] + 0.0593 * b[2];
    (y.is_finite() && y > 1e-6 && b.iter().all(|v| *v > 1e-6)).then(|| b.map(|v| y / v))
}

/// White balance only (exposure is applied by the per-pixel stage, see [`crate::Prepared`]).
pub fn white_balance(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 1.0);
}

/// The white-balance matrix (linear Rec.2020) for the settings, or `None` when the as-shot white
/// is kept. A raw source with its camera colour model ([`crate::CameraColor`]) is re-developed for
/// the new white in camera space, as Lightroom does (so a neutral under the chosen white renders
/// neutral and saturated colours move as the camera's matrices say); other sources are adapted
/// with Bradford, luminance-preserving.
///
/// A film negative with an explicit white balance is the exception: its white balance is the
/// film-base neutralization ([`negative_base_gains`]), a diagonal matrix, and the Kelvin value
/// plays no part. `wb.mode == AsShot` keeps the scan as it came — the confirmed invert window
/// already neutralizes the base per channel, and the reference fixtures pin that path.
pub fn wb_matrix_for(info: &SourceInfo, s: &DevelopSettings) -> Option<[[f32; 3]; 3]> {
    if s.negative.enabled && s.section_enabled("negative") && s.wb.mode != lightcraft_develop::WbMode::AsShot {
        let gains = s.negative.has_base().then(|| negative_base_gains([s.negative.base_r, s.negative.base_g, s.negative.base_b])).flatten();
        return gains.map(|g| lightcraft_color::Mat3::diag(g[0], g[1], g[2]).to_f32());
    }
    let (t, tint) = effective_wb(info, s);
    if (t - info.as_shot_temp).abs() < 1e-6 && (tint - info.as_shot_tint).abs() < 1e-6 {
        return None;
    }
    if let Some(cc) = info.camera_color.as_deref().filter(|_| info.raw && !info.relative_wb)
        && let Some(m) = lightcraft_raw::color::rebalance(&cc.tags, cc.developed_for, temp_tint_to_xy(t, tint))
    {
        return Some(m.to_f32());
    }
    let set = wb_matrix(&REC2020, effective_white(info, s));
    let shot = wb_matrix(&REC2020, temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint));
    let m = set.mul(&shot.inverse().unwrap_or(lightcraft_color::Mat3::IDENTITY));
    // Normalize so neutral luminance is preserved (WB shouldn't change exposure).
    let g = m.apply([1.0, 1.0, 1.0]);
    let y = g[0] * 0.2627 + g[1] * 0.6780 + g[2] * 0.0593;
    Some(m.mul(&lightcraft_color::Mat3::diag(1.0 / y, 1.0 / y, 1.0 / y)).to_f32())
}

fn wb_gain(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings, gain: f32) {
    let m = wb_matrix_for(info, s);
    let w = img.width;
    for_rows(&mut img.data, w, |_, row| {
        for p in row.iter_mut() {
            let mut c = *p;
            if let Some(m) = &m {
                c = [
                    m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
                    m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
                    m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
                ];
            }
            *p = [(c[0] * gain).max(0.0), (c[1] * gain).max(0.0), (c[2] * gain).max(0.0)];
        }
    });
}

/// The white balance actually in effect (presets resolve to their Kelvin values for raw files; settings written on
/// the relative scale before a raw's camera matrices reached its white balance give the same colour change on the
/// Kelvin scale, see [`legacy_to_kelvin`]).
pub fn effective_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    use lightcraft_develop::WbMode;
    match s.wb.mode {
        WbMode::AsShot => (info.as_shot_temp, info.as_shot_tint),
        _ if legacy_wb(info, s) => legacy_to_kelvin(s.wb.temp, s.wb.tint, (info.as_shot_temp, info.as_shot_tint)),
        m if info.raw && !info.relative_wb => m.preset().unwrap_or((s.wb.temp, s.wb.tint)),
        _ => (s.wb.temp, s.wb.tint),
    }
}

/// The source white the render adapts from the as-shot white to: [`effective_wb`] as a chromaticity, except that a
/// white balance still on the relative scale is composed exactly ([`legacy_white`]) rather than read back through
/// its rounded Kelvin / tint.
fn effective_white(info: &SourceInfo, s: &DevelopSettings) -> lightcraft_color::Xy {
    if legacy_wb(info, s)
        && s.wb.mode != lightcraft_develop::WbMode::AsShot
        && let Some(xy) = legacy_white(s.wb.temp, s.wb.tint, (info.as_shot_temp, info.as_shot_tint))
    {
        return xy;
    }
    let (t, tint) = effective_wb(info, s);
    temp_tint_to_xy(t, tint)
}

/// Whether `s` holds a white balance written on the relative scale for a source that is edited in Kelvin today.
fn legacy_wb(info: &SourceInfo, s: &DevelopSettings) -> bool {
    info.raw && !info.relative_wb && info.legacy_relative_wb && s.wb.scale == lightcraft_develop::WbScale::Legacy
}

/// The white that renders exactly as `(temp, tint)` did on the relative scale, where 6500 K / 0 meant *as shot* and
/// the source's real as-shot white is `as_shot`.
///
/// The relative render adapted the picture from 6500 K / 0 to `(temp, tint)` (Bradford, [`wb_matrix_for`]); the
/// Kelvin render adapts it from `as_shot` to the result. Bradford adaptations are von Kries scalings in one cone
/// space, so they compose exactly: the new white's cone response is the old target's times the as-shot white's
/// over the old reference's, and the two renders give the same matrix (up to the scale the luminance
/// normalisation removes). `None` when the arithmetic has nowhere to go (a non-finite input, a white outside the
/// chromaticity plane).
pub fn legacy_white(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<lightcraft_color::Xy> {
    use lightcraft_color::{BRADFORD, Xy};
    if !(temp.is_finite() && tint.is_finite() && as_shot.0.is_finite() && as_shot.1.is_finite()) {
        return None;
    }
    let cones = |t: f64, tint: f64| BRADFORD.apply(temp_tint_to_xy(t, tint).to_xyz());
    let (old, reference, shot) = (cones(temp, tint), cones(6500.0, 0.0), cones(as_shot.0, as_shot.1));
    let inverse = BRADFORD.inverse()?;
    let mut new = [0.0; 3];
    for i in 0..3 {
        if reference[i].abs() < 1e-12 {
            return None;
        }
        new[i] = old[i] * shot[i] / reference[i];
    }
    let xy = Xy::from_xyz(inverse.apply(new));
    (xy.x.is_finite() && xy.y.is_finite() && xy.y > 0.0).then_some(xy)
}

/// The Kelvin / tint of [`legacy_white`]: what a white balance written on the relative scale reads as, and becomes
/// when it is next edited ([`normalize_wb`]). Exact wherever the white has a Kelvin value (1000–50000 K after the
/// composition; the relative slider reached 2000–13550 K around 6500 K, so only a far-blue as-shot white pushed
/// to the slider's blue end lands beyond, and clamps), to the precision of the Kelvin / tint parametrisation
/// (better than 0.01 % away from its 4000 K seam). Unchanged when the arithmetic has nowhere to go.
pub fn legacy_to_kelvin(temp: f64, tint: f64, as_shot: (f64, f64)) -> (f64, f64) {
    if temp == 6500.0 && tint == 0.0 && as_shot.0.is_finite() && as_shot.1.is_finite() {
        // the relative scale's as-shot reference, exactly (a Custom value left where As Shot was stays as shot)
        return as_shot;
    }
    match legacy_white(temp, tint, as_shot).map(xy_to_temp_tint) {
        Some((t, tint)) if t.is_finite() && tint.is_finite() => (t, tint),
        _ => (temp, tint),
    }
}

/// Bring the white balance of `d` onto the scale `info` is edited on, before a new value is written into it: on a
/// raw with a Kelvin scale the values are marked Kelvin, and ones still on the relative scale become the Custom
/// Kelvin / tint that renders the same ([`legacy_to_kelvin`]; a preset mode's look is kept as that custom
/// value, since the preset's Kelvin value would be a different look). Elsewhere the scale is cleared, so values
/// edited on a rendered photo or a raw without matrices never pass for Kelvin when pasted onto one that has them.
pub fn normalize_wb(d: &mut DevelopSettings, info: &SourceInfo) {
    use lightcraft_develop::{WbMode, WbScale};
    if !(info.raw && !info.relative_wb) {
        d.wb.scale = WbScale::Legacy;
        return;
    }
    if legacy_wb(info, d) && d.wb.mode != WbMode::AsShot {
        let (t, tint) = legacy_to_kelvin(d.wb.temp, d.wb.tint, (info.as_shot_temp, info.as_shot_tint));
        d.wb.temp = t;
        d.wb.tint = tint;
        d.wb.mode = WbMode::Custom;
    }
    d.wb.scale = WbScale::Kelvin;
}

/// Self-guided filter (He et al.) on a single plane with Gaussian windows of `sigma` px.
pub fn guided(p: &Plane, sigma: f32, eps: f32) -> Plane {
    let (ma, mb) = guided_coeffs(p, sigma, eps);
    guided_apply(p, &ma, &mb)
}

/// Blurred guided-filter coefficients (mean a, mean b) of `p`; every pass row-parallel, and the
/// two independent blurs of each step run side by side.
fn guided_coeffs(p: &Plane, sigma: f32, eps: f32) -> (Plane, Plane) {
    let (mean, corr) = par_join(|| gaussian(p, sigma), || gaussian(&p.map(|v| v * v), sigma));
    let a = corr.zip_map(&mean, |c, m| {
        let var = (c - m * m).max(0.0);
        var / (var + eps)
    });
    let b = mean.zip_map(&a, |m, a| m - a * m);
    par_join(|| gaussian(&a, sigma), || gaussian(&b, sigma))
}

/// `q = a·p + b` per pixel.
fn guided_apply(p: &Plane, a: &Plane, b: &Plane) -> Plane {
    let mut q = Plane::new(p.width, p.height);
    let w = p.width;
    for_rows(&mut q.data, w, |y, row| {
        for ((v, pv), (av, bv)) in row.iter_mut().zip(p.row(y)).zip(a.row(y).iter().zip(b.row(y))) {
            *v = av * pv + bv;
        }
    });
    q
}

/// Fast guided filter (He & Sun 2015): coefficients computed on a `s×` subsampled plane and
/// bilinearly upsampled; the output keeps full-resolution edges. Equivalent to [`guided`] for
/// large windows at a fraction of the cost.
pub fn guided_fast(p: &Plane, sigma: f32, eps: f32) -> Plane {
    use lightcraft_raster::resample::{Filter, resize};
    let s = guided_fast_step(sigma);
    if s <= 1 {
        return guided(p, sigma, eps);
    }
    let (lw, lh) = (p.width.div_ceil(s).max(1), p.height.div_ceil(s).max(1));
    let lo = resize(p, lw, lh, Filter::Box);
    let (ma, mb) = guided_coeffs(&lo, sigma / s as f32, eps);
    let up = |c: &Plane| resize(c, p.width, p.height, Filter::Bilinear);
    guided_apply(p, &up(&ma), &up(&mb))
}

/// Separable median (`2r+1` px along each row, then each column; clamped edges). It keeps the
/// steps between regions where they are and removes features thinner than about `r` px.
pub fn median_hv(p: &Plane, r: usize) -> Plane {
    if r == 0 || p.data.is_empty() {
        return p.clone();
    }
    let rows = |src: &Plane| {
        let mut out = Plane::new(src.width, src.height);
        for_rows(&mut out.data, src.width, |y, row| median_row(src.row(y), row, r));
        out
    };
    transpose(&rows(&transpose(&rows(p))))
}

/// Running median of `src` over `2r+1` taps (clamped edges) into `out`: a sorted window that
/// drops the leaving value and inserts the entering one.
fn median_row(src: &[f32], out: &mut [f32], r: usize) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize];
    let r_ = r as isize;
    let mut win: Vec<f32> = (-r_..=r_).map(at).collect();
    win.sort_unstable_by(f32::total_cmp);
    for (x, o) in out.iter_mut().enumerate() {
        *o = win[r];
        let (old, new) = (at(x as isize - r_), at(x as isize + r_ + 1));
        if old.to_bits() != new.to_bits() {
            let i = win.binary_search_by(|v| v.total_cmp(&old)).unwrap_or_else(|i| i.min(win.len() - 1));
            win.remove(i);
            let j = win.binary_search_by(|v| v.total_cmp(&new)).unwrap_or_else(|j| j);
            win.insert(j, new);
        }
    }
}

fn transpose(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let mut t = Plane::new(h, w);
    for_rows(&mut t.data, h, |x, row| {
        for (y, v) in row.iter_mut().enumerate() {
            *v = p.data[y * w + x];
        }
    });
    t
}

/// Detail scale of the highlights/shadows base from process V2, as a fraction of the long edge:
/// the base is the edge-aware filter of the [`median_hv`] of log luminance at this radius, so
/// twigs, lines and fine texture brighter or darker than their surroundings take their
/// surroundings' shift (and keep their contrast, as with Exposure) while region boundaries stay
/// sharp (no halos). V1's base is the edge-aware filter of log luminance itself (issue #632).
pub const BASE_DETAIL: f64 = 0.0015;
/// Largest [`base_detail_radius`] (px).
pub const BASE_DETAIL_MAX: usize = 24;

/// Median radius (px) of the highlights/shadows base at `px_per_long` (see [`BASE_DETAIL`]).
pub fn base_detail_radius(px_per_long: f64) -> usize {
    ((BASE_DETAIL * px_per_long).round().max(0.0) as usize).min(BASE_DETAIL_MAX)
}

/// Median radius (px) of the highlights/shadows base of `s`'s rendering process at `px_per_long`:
/// 0 (no median) under V1, [`base_detail_radius`] from V2.
pub fn tone_base_detail(s: &DevelopSettings, px_per_long: f64) -> usize {
    match s.process.process() {
        Process::V1 => 0,
        Process::V2 => base_detail_radius(px_per_long),
    }
}

/// The highlights/shadows base layer of log luminance `l`: the fast guided filter ([`BASE_EPS`])
/// of its [`median_hv`] at `detail` px ([`tone_base_detail`]); at 0, of `l` itself (V1).
pub fn tone_base(l: &Plane, sigma: f32, detail: usize) -> Plane {
    if detail == 0 {
        return guided_fast(l, sigma, BASE_EPS);
    }
    guided_fast(&median_hv(l, detail), sigma, BASE_EPS)
}

/// Subsampling step of [`guided_fast`] for `sigma` (1 = no subsampling: the plain guided filter).
pub fn guided_fast_step(sigma: f32) -> usize {
    (sigma / 3.0).floor().clamp(1.0, 16.0) as usize
}

/// Luminance noise reduction: guided filter of log luminance (`sigma` px, `eps`), blended by `k`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrLum {
    pub sigma: f32,
    pub eps: f32,
    pub k: f32,
}

/// Colour noise reduction: the R/B colour differences from green (in square-root chroma) are
/// edge-aware smoothed by `passes` self-guided passes of `sigma` px at `eps` and blended back by
/// `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrColor {
    pub sigma: f32,
    pub passes: usize,
    pub eps: f32,
    pub t: f32,
}

/// Offset of the square roots in the chroma basis; keeps the derivative finite near black.
pub const CHROMA_OFF: f32 = 0.003;

/// Chroma smoothing strength relative to the measured chroma-noise variance: `eps = STRENGTH·var`.
/// The variance is a low quantile of the local chroma variance, i.e. the flat-area noise level.
pub const CNR_NOISE_STRENGTH: f32 = 2.0;

/// Self-guided passes per render. Two passes at one radius reach a low residual with less edge
/// bleed than one pass at a wider radius (which would need a larger `eps` to smooth as much).
pub const CNR_PASSES: usize = 2;

/// The R/B colour differences from green in square-root chroma (the basis the colour noise filter
/// works on): the noise is about the same size at every brightness and nothing blows up near black.
#[inline]
pub fn chroma_sqrt(c: [f32; 3]) -> [f32; 2] {
    let g = (c[1] + CHROMA_OFF).max(0.0).sqrt();
    [(c[0] + CHROMA_OFF).max(0.0).sqrt() - g, (c[2] + CHROMA_OFF).max(0.0).sqrt() - g]
}

/// A robust flat-area level of the chroma noise: the 20th percentile of the local (square-root
/// chroma) variance over the source. The colour noise reduction sizes its filter from it, and the
/// CPU and GPU renderers share the value (both start from the same source).
pub fn chroma_noise(src: &Rgb32f) -> f32 {
    let (w, h) = (src.width, src.height);
    if w < 8 || h < 8 {
        return 0.0;
    }
    let step = ((w.min(h) / 256).max(1) * 2).max(1);
    let mut vars: Vec<f32> = Vec::new();
    let mut y = 2;
    while y + 2 < h {
        let mut x = 2;
        while x + 2 < w {
            let (mut s, mut sq) = ([0.0f32; 2], [0.0f32; 2]);
            for dy in 0..5 {
                for dx in 0..5 {
                    let d = chroma_sqrt(src.data[(y + dy - 2) * w + (x + dx - 2)]);
                    for k in 0..2 {
                        s[k] += d[k];
                        sq[k] += d[k] * d[k];
                    }
                }
            }
            for k in 0..2 {
                let m = s[k] / 25.0;
                vars.push((sq[k] / 25.0 - m * m).max(0.0));
            }
            x += step;
        }
        y += step;
    }
    if vars.len() < 16 {
        return 0.0;
    }
    vars.sort_by(f32::total_cmp);
    vars[(vars.len() - 1) * 20 / 100]
}

/// A robust flat-area level of the luminance noise: the 20th percentile of the local log-luminance
/// variance over the source (EV²), measured like [`chroma_noise`]. The luminance noise reduction
/// sizes its filter from it, so a low-ISO file keeps its detail and a dark high-ISO one is calmed.
pub fn luma_noise(src: &Rgb32f) -> f32 {
    let (w, h) = (src.width, src.height);
    if w < 8 || h < 8 {
        return 0.0;
    }
    let step = ((w.min(h) / 256).max(1) * 2).max(1);
    let mut vars: Vec<f32> = Vec::new();
    let mut y = 2;
    while y + 2 < h {
        let mut x = 2;
        while x + 2 < w {
            let (mut s, mut sq) = (0.0f32, 0.0f32);
            for dy in 0..5 {
                for dx in 0..5 {
                    let v = log_lum(src.data[(y + dy - 2) * w + (x + dx - 2)]);
                    s += v;
                    sq += v * v;
                }
            }
            let m = s / 25.0;
            vars.push((sq / 25.0 - m * m).max(0.0));
            x += step;
        }
        y += step;
    }
    if vars.len() < 16 {
        return 0.0;
    }
    vars.sort_by(f32::total_cmp);
    vars[(vars.len() - 1) * 20 / 100]
}

/// Luminance smoothing strength relative to the measured luminance-noise variance: `eps =
/// STRENGTH·var`. The variance is a low quantile of the local log-luminance variance (the
/// flat-area noise level), so the same slider calms a high-ISO frame and leaves a low-ISO one.
pub const LNR_NOISE_STRENGTH: f32 = 0.85;

/// Noise-reduction parameters at an output long edge of `out_long` px (see [`denoise`]).
/// `chroma` and `luma` are [`chroma_noise`] and [`luma_noise`] of the source.
pub fn nr_params(s: &DevelopSettings, src_long: usize, out_long: usize, chroma: f32, luma: f32) -> (Option<NrLum>, Option<NrColor>) {
    let lum = (s.detail.nr_luminance / 100.0) as f32;
    let col = (s.detail.nr_color / 100.0) as f32;
    let scale = (out_long as f32 / src_long.max(1) as f32).clamp(0.05, 1.0);
    let l = (lum > 0.0).then(|| {
        let detail = (s.detail.nr_detail / 100.0) as f32;
        // eps follows the luminance noise measured in the photo's flat areas, like the colour NR:
        // a low-ISO file keeps its detail and a dark high-ISO one is actually calmed. Detail keeps
        // fine luminance back by lowering eps; the slider sets the radius and how much of the
        // smoothed luminance is written back. The radius starts at ~2 px so that even a low
        // Luminance setting reaches the coarse luminance grain, not just fine speckle.
        let n2 = (luma * scale * scale).max(1e-12);
        let eps = LNR_NOISE_STRENGTH * (1.5 - detail) * n2;
        NrLum { sigma: (2.0 + 4.0 * lum) * scale.max(0.4), eps, k: lum.sqrt() }
    });
    let c = (col > 0.0).then(|| {
        let smooth = (s.detail.nr_color_smoothness / 100.0) as f32;
        let detail = (s.detail.nr_color_detail / 100.0) as f32;
        let sigma = (3.0 + 7.0 * col) * scale.max(0.35) * (1.0 + smooth);
        // eps follows the noise measured in the photo (its flat areas), not a fixed number, so a
        // low-ISO file keeps its colour detail and a dark high-ISO one is actually calmed (a fixed
        // blur, by contrast, leaves coarse chroma behind). Detail keeps fine colour back by
        // lowering eps. The Color slider sets the radius and how much of the smoothed chroma is
        // written back — not eps — so raising it smooths further without bleeding more across a
        // colour edge. Two passes at one radius reach a low residual.
        let n2 = (chroma * scale * scale).max(1e-12);
        let eps = CNR_NOISE_STRENGTH * (1.5 - detail) * n2;
        let t = (col / 0.25).clamp(0.0, 1.0);
        NrColor { sigma, passes: CNR_PASSES, eps, t }
    });
    (l, c)
}

/// Noise reduction at output resolution: luminance via an edge-aware self-guided filter on
/// log-luminance, colour by edge-aware smoothing the square-root R/B colour differences from green
/// (green and the pixel's luminance are untouched). Radii scale with how much the source was
/// downsampled (preview noise is already averaged out).
pub fn denoise(img: &mut Rgb32f, s: &DevelopSettings, src_long: usize, out_long: usize, chroma: f32, luma: f32) {
    let (lum, col) = nr_params(s, src_long, out_long, chroma, luma);
    let w = img.width;
    if let Some(nr) = lum {
        let _t = crate::profiling().then(std::time::Instant::now);
        let l = img.map(log_lum);
        let f = guided(&l, nr.sigma, nr.eps);
        for_rows(&mut img.data, w, |y, row| {
            for (x, p) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let d = (f.data[i] - l.data[i]) * nr.k;
                let g = d.exp2();
                *p = p.map(|v| v * g);
            }
        });
        if let Some(t) = _t {
            eprintln!("    nr luminance: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
    if let Some(nr) = col {
        let _t = crate::profiling().then(std::time::Instant::now);
        // Colour noise lives in the R/B colour differences from green (each R/B site is
        // interpolated from a quarter of the samples, green from half). A self-guided filter of
        // those differences removes the noise in flat areas while keeping colour edges — so the
        // noise is removed instead of being re-applied to every channel, and no colour bleeds
        // across an edge (a plain blur fringes at high Color values; a luminance `rgb / Y` basis
        // leaks noise back as channel noise and blows up in the shadows).
        let dr = img.map(|c| chroma_sqrt(c)[0]);
        let db = img.map(|c| chroma_sqrt(c)[1]);
        let smooth = |p: &Plane| {
            let mut q = guided_fast(p, nr.sigma, nr.eps);
            for _ in 1..nr.passes {
                q = guided_fast(&q, nr.sigma, nr.eps);
            }
            q
        };
        let qr = smooth(&dr);
        let qb = smooth(&db);
        let gs = img.map(|c| (c[1] + CHROMA_OFF).max(0.0).sqrt());
        for_rows(&mut img.data, w, |y, row| {
            for (x, p) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let r = (gs.data[i] + dr.data[i] + (qr.data[i] - dr.data[i]) * nr.t).max(0.0);
                let b = (gs.data[i] + db.data[i] + (qb.data[i] - db.data[i]) * nr.t).max(0.0);
                p[0] = (r * r - CHROMA_OFF).max(0.0);
                p[2] = (b * b - CHROMA_OFF).max(0.0);
            }
        });
        if let Some(t) = _t {
            eprintln!("    nr colour: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
}

pub fn log_lum(c: [f32; 3]) -> f32 {
    (luminance_2020(c).max(1e-7) / crate::tone::GREY).log2()
}

/// Spatial planes of one (pre-exposure) image, each tagged with the radius it was computed at.
/// `key` identifies the image; see [`crate::StageCache`].
#[derive(Clone, Default)]
pub(crate) struct Planes {
    pub key: u64,
    /// The image's key without its noise reduction (`Plan::pre_nr_key`).
    pub pre_nr: u64,
    pub log_l: Option<Arc<Plane>>,
    /// Keyed by its radius, process and median radius ([`BaseKey`]).
    pub base: Option<(BaseKey, Arc<Plane>)>,
    pub clarity: Option<(u32, Arc<Plane>)>,
    pub texture: Option<(u32, Arc<Plane>)>,
    /// Dark channel and its airlight.
    pub dark: Option<(u32, Arc<Plane>, f32)>,
    /// Blurred chromaticity (local Moiré / Noise).
    pub chroma: Option<(u32, Arc<Rgb32f>)>,
}

impl Planes {
    /// The planes that still fit an image differing from this one only in noise reduction, close
    /// enough for a draft: the wide blurs (base, clarity, dehaze, chromaticity). Its `key` matches
    /// no image, so only another draft takes them.
    pub fn for_other_nr(self) -> Planes {
        Planes { key: 0, pre_nr: self.pre_nr, log_l: None, texture: None, ..self }
    }
}

/// What the highlights/shadows base plane was computed with: its radius (`f32` bits), the
/// rendering process and the median radius ([`tone_base_detail`]). The base differs between
/// processes, so a photo moved to another one (Update to Current Process, undo) recomputes it.
pub type BaseKey = (u32, Process, usize);

/// The [`BaseKey`] of a base at `sigma` for `s` (`detail`: [`PlaneSigmas::base_detail`]).
pub fn base_key(s: &DevelopSettings, sigma: f32, detail: usize) -> BaseKey {
    (sigma.to_bits(), s.process.process(), detail)
}

/// Reuse `slot` if it was computed with `key` (a radius's `f32` bits, or a [`BaseKey`]), else
/// compute and store it.
pub fn plane_at<K: Copy + PartialEq, P>(slot: &mut Option<(K, Arc<P>)>, key: K, f: impl FnOnce() -> P) -> Arc<P> {
    match slot {
        Some((k, p)) if *k == key => p.clone(),
        _ => {
            let p = Arc::new(f());
            *slot = Some((key, p.clone()));
            p
        }
    }
}

/// Guided-filter epsilons (EV²) of the highlights/shadows base and the clarity band.
pub const BASE_EPS: f32 = 0.35;
pub const CLARITY_EPS: f32 = 0.8;

/// Dark channel of a pixel (dehaze).
#[inline]
pub fn dark_of(c: [f32; 3]) -> f32 {
    c[0].min(c[1]).min(c[2])
}

/// Radius of the local Moiré / colour-noise chromaticity blur, as a fraction of the long edge.
pub const CHROMA_SIGMA: f32 = 0.004;

/// Radii (px) of the spatial planes the settings need (`None` = not needed).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlaneSigmas {
    /// Edge-aware base for highlights/shadows ([`tone_base`]: fast guided filter, [`BASE_EPS`]; from
    /// process V2 of the [`BASE_DETAIL`] median).
    pub base: Option<f32>,
    /// Median radius (px) of that base ([`tone_base_detail`]: 0 under V1).
    pub base_detail: usize,
    /// Clarity band (fast guided filter, [`CLARITY_EPS`]).
    pub clarity: Option<f32>,
    /// Texture / sharpening band (Gaussian).
    pub texture: Option<f32>,
    /// Dehaze dark channel (Gaussian).
    pub dark: Option<f32>,
    /// Chromaticity blur for local Moiré / colour noise (Gaussian).
    pub chroma: Option<f32>,
}

pub fn plane_sigmas(s: &DevelopSettings, px_per_long: f64, q: Quality) -> PlaneSigmas {
    let ppl = px_per_long as f32;
    let local_any = |f: fn(&lightcraft_develop::LocalAdjustments) -> f64| s.masks.iter().any(|m| f(&m.adjust) != 0.0);
    let tone_active =
        s.light.highlights != 0.0 || s.light.shadows != 0.0 || s.masks.iter().any(|m| m.adjust.highlights != 0.0 || m.adjust.shadows != 0.0);
    // Edge-aware base at ~1.5% of the long edge (EV² epsilon: edges of > ~0.6 EV are preserved)
    // From V2 it is taken of the median at 0.15%: lines and texture finer than that are detail,
    // not tone (#632).
    let base = tone_active.then(|| {
        let sigma = (0.015 * ppl).max(1.0);
        if q == Quality::Draft { sigma.min(24.0) } else { sigma }
    });
    let clarity = (s.effects.clarity != 0.0 || local_any(|a| a.clarity)).then(|| (0.012 * ppl).max(1.0));
    // local Noise and Defringe read the fine detail band too
    let texture = (s.effects.texture != 0.0
        || s.detail.sharpen_amount != 0.0
        || local_any(|a| a.texture)
        || local_any(|a| a.sharpness)
        || local_any(|a| a.noise)
        || s.masks.iter().any(|m| m.adjust.defringe > 0.0))
    .then(|| (0.0018 * ppl).max(0.6));
    let dark = (s.effects.dehaze != 0.0 || local_any(|a| a.dehaze)).then(|| (0.02 * ppl).max(1.0));
    let chroma = (local_any(|a| a.moire) || s.masks.iter().any(|m| m.adjust.noise > 0.0)).then(|| (CHROMA_SIGMA * ppl).max(1.0));
    let base_detail = if base.is_some() { tone_base_detail(s, px_per_long) } else { 0 };
    PlaneSigmas { base, base_detail, clarity, texture, dark, chroma }
}

/// The spatial planes the per-pixel stage needs for `img` (white-balanced, before exposure),
/// reusing whatever `planes` already holds for it. Missing planes are computed side by side (each
/// one alone scales poorly: the guided filters work on small subsampled grids).
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    img: Arc<Rgb32f>,
    s: &DevelopSettings,
    frame: &Frame,
    px_per_long: f64,
    q: Quality,
    planes: &mut Planes,
    mattes: Option<&masks::Mattes>,
) -> Prepared {
    let log_l = planes.log_l.get_or_insert_with(|| Arc::new(timed("log_l", || img.map(log_lum)))).clone();
    let PlaneSigmas { base: base_sigma, base_detail, clarity: clarity_sigma, texture: texture_sigma, dark: dark_sigma, chroma: chroma_sigma } =
        plane_sigmas(s, px_per_long, q);

    let Planes { base: sb, clarity: sc, texture: st, dark: sd, chroma: sch, .. } = planes;
    let chroma_blur = chroma_sigma.map(|sg| match sch {
        Some((k, c)) if *k == sg.to_bits() => c.clone(),
        _ => {
            let c = timed("chroma", || Arc::new(gaussian(&img.map(masks::chromaticity), sg)));
            *sch = Some((sg.to_bits(), c.clone()));
            c
        }
    });
    let l = &log_l;
    let (base, (clarity_blur, (texture_blur, dark))) = par_join(
        || base_sigma.map(|sg| plane_at(sb, base_key(s, sg, base_detail), || timed("base", || tone_base(l, sg, base_detail)))),
        || {
            par_join(
                || clarity_sigma.map(|sg| plane_at(sc, sg.to_bits(), || timed("clarity", || guided_fast(l, sg, CLARITY_EPS)))),
                || {
                    par_join(
                        || texture_sigma.map(|sg| plane_at(st, sg.to_bits(), || timed("texture", || gaussian(l, sg)))),
                        || {
                            dark_sigma.map(|sg| match sd {
                                Some((k, d, air)) if *k == sg.to_bits() => (d.clone(), *air),
                                _ => {
                                    let d = timed("dehaze", || Arc::new(gaussian(&img.map(dark_of), sg)));
                                    let air = airlight(&d);
                                    *sd = Some((sg.to_bits(), d.clone(), air));
                                    (d, air)
                                }
                            })
                        },
                    )
                },
            )
        },
    );
    let base = base.unwrap_or_else(|| log_l.clone());
    let (dark, air) = match dark {
        Some((d, air)) => (Some(d), air),
        None => (None, 1.0),
    };
    let ev = s.light.exposure as f32;
    let masks = timed("masks", || masks::evaluate(&s.masks, frame, img.width, img.height, &img, &log_l, ev, mattes));
    Prepared { img, log_l, base, clarity_blur, texture_blur, dark, chroma_blur, air, masks, px_per_long }
}

/// The airlight of the whole output frame, estimated on a small render of it (`proxy_w` px wide):
/// a windowed render must dehaze with the frame's airlight, not that of the pixels it holds.
pub(crate) fn frame_airlight(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, proxy_w: usize, proxy_h: usize) -> f32 {
    let mut img = frame.sample(src, proxy_w, proxy_h);
    white_balance(&mut img, info, s);
    let sigma = (0.02 * frame.px_per_long(proxy_w)).max(1.0) as f32;
    airlight(&gaussian(&img.map(dark_of), sigma))
}

/// The airlight is estimated from every `AIRLIGHT_STEP`-th value of the dark channel.
pub const AIRLIGHT_STEP: usize = 7;

/// Airlight estimate: bright end of the dark channel.
pub fn airlight(dark: &Plane) -> f32 {
    airlight_of(dark.data.iter().step_by(AIRLIGHT_STEP).copied().collect())
}

/// Airlight from the sampled dark-channel values (see [`airlight`]): their 99.5th percentile.
pub fn airlight_of(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() as f32) * 0.995) as usize;
    let k = k.min(v.len() - 1);
    v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    v[k].max(0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_preserves_step_edges_and_flattens_texture() {
        let p = Plane::from_fn(80, 20, |x, y| if x < 40 { -2.0 } else { 2.0 } + if (x + y) % 2 == 0 { 0.05 } else { -0.05 });
        let q = guided(&p, 4.0, 0.3);
        // edge kept
        assert!(q.get(35, 10) < -1.5 && q.get(45, 10) > 1.5);
        // checkerboard texture removed away from the edge
        assert!((q.get(10, 10) - q.get(11, 10)).abs() < 0.02);
    }

    #[test]
    fn wb_as_shot_is_identity_and_warming_adds_red() {
        let s = DevelopSettings::default();
        let info = SourceInfo::default();
        let mut img = Rgb32f::filled(4, 4, [0.3, 0.3, 0.3]);
        scene_linear_pre(&mut img, &info, &s);
        assert!((img.get(0, 0)[0] - 0.3).abs() < 1e-6);
        let mut warm = s.clone();
        warm.wb.mode = lightcraft_develop::WbMode::Custom;
        warm.wb.temp = 9000.0;
        let mut img2 = Rgb32f::filled(4, 4, [0.3, 0.3, 0.3]);
        scene_linear_pre(&mut img2, &info, &warm);
        let c = img2.get(0, 0);
        assert!(c[0] > c[2], "{c:?}");
        // luminance preserved
        assert!((luminance_2020(c) - 0.3).abs() < 0.01);
    }

    /// The negative white balance is the film-base neutralization: an explicit white balance on a
    /// negative sends the measured base to a grey at the base's own luminance, whatever Kelvin the
    /// settings carry, and `AsShot` keeps the scan (the reference fixtures pin that path).
    #[test]
    fn negative_explicit_wb_neutralizes_the_film_base() {
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        (s.negative.base_r, s.negative.base_g, s.negative.base_b) = (0.84, 0.66, 0.47);
        let info = SourceInfo::default();
        assert!(wb_matrix_for(&info, &s).is_none(), "AsShot keeps the scan");
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = 2300.0;
        s.wb.tint = -15.0;
        let m = wb_matrix_for(&info, &s).unwrap();
        let b = lightcraft_negative::srgb_encoded_to_linear_rec2020([0.84, 0.66, 0.47]);
        let w = [
            m[0][0] * b[0] + m[0][1] * b[1] + m[0][2] * b[2],
            m[1][0] * b[0] + m[1][1] * b[1] + m[1][2] * b[2],
            m[2][0] * b[0] + m[2][1] * b[1] + m[2][2] * b[2],
        ];
        assert!(w.iter().all(|v| (*v - w[0]).abs() < 1e-5), "the base must come out neutral: {w:?}");
        let y0 = luminance_2020(b);
        assert!((luminance_2020(w) - y0).abs() < 1e-5, "the base's luminance must be kept: {w:?} vs {y0}");
        // a warm Kelvin value must not leak into the matrix: 6500 K is the same neutralization
        s.wb.temp = 6500.0;
        let m2 = wb_matrix_for(&info, &s).unwrap();
        assert_eq!(m, m2, "the Kelvin value plays no part on a negative");
    }

    /// The invert window's high end is the white-balanced base: after the neutralization the base
    /// maps to black even though the window was measured on the as-shot scan.
    #[test]
    fn the_invert_window_uses_the_white_balanced_base() {
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        (s.negative.base_r, s.negative.base_g, s.negative.base_b) = (0.84, 0.66, 0.47);
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = 2300.0;
        let base_lin = lightcraft_negative::srgb_encoded_to_linear_rec2020([0.84, 0.66, 0.47]);
        let mut img = Rgb32f::filled(2, 1, base_lin);
        let info = SourceInfo::default();
        let m = wb_matrix_for(&info, &s).unwrap();
        white_balance(&mut img, &info, &s);
        let w = [
            m[0][0] * base_lin[0] + m[0][1] * base_lin[1] + m[0][2] * base_lin[2],
            m[1][0] * base_lin[0] + m[1][1] * base_lin[1] + m[1][2] * base_lin[2],
            m[2][0] * base_lin[0] + m[2][1] * base_lin[1] + m[2][2] * base_lin[2],
        ];
        let w_enc = lightcraft_negative::linear_rec2020_to_srgb_encoded(w);
        (s.negative.base_r, s.negative.base_g, s.negative.base_b) = (w_enc[0] as f64, w_enc[1] as f64, w_enc[2] as f64);
        crate::negative::apply(&mut img, &s);
        let back = lightcraft_negative::linear_rec2020_to_srgb_encoded(img.data[0]);
        for c in 0..3 {
            assert!(back[c] < 0.02, "the neutralized base must invert to black, chan {c}: {back:?}");
        }
    }

    #[test]
    fn exposure_doubles() {
        let mut s = DevelopSettings::default();
        s.light.exposure = 1.0;
        let mut img = Rgb32f::filled(2, 2, [0.1, 0.2, 0.3]);
        scene_linear_pre(&mut img, &SourceInfo::default(), &s);
        assert!((img.get(1, 1)[2] - 0.6).abs() < 1e-5);
    }
}

#[cfg(test)]
mod fast_tests {
    use super::*;

    #[test]
    fn fast_guided_close_to_reference() {
        let p = Plane::from_fn(160, 120, |x, y| ((x as f32 * 0.13).sin() + (y as f32 * 0.07).cos()) * 2.0 + if x > 80 { 1.5 } else { 0.0 });
        let a = guided(&p, 12.0, 0.3);
        let b = guided_fast(&p, 12.0, 0.3);
        let err: f32 = a.data.iter().zip(&b.data).map(|(u, v)| (u - v).abs()).sum::<f32>() / a.len() as f32;
        assert!(err < 0.08, "{err}");
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;

    #[test]
    fn luminance_nr_reduces_noise_keeps_mean() {
        let mut img = Rgb32f::from_fn(64, 64, |x, y| {
            let n = (((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5) * 0.06;
            [0.2 + n; 3]
        });
        let var = |im: &Rgb32f| {
            let m = im.data.iter().map(|p| p[1]).sum::<f32>() / im.len() as f32;
            (im.data.iter().map(|p| (p[1] - m).powi(2)).sum::<f32>() / im.len() as f32, m)
        };
        let (v0, m0) = var(&img);
        let mut s = DevelopSettings::default();
        s.detail.nr_luminance = 80.0;
        let n = luma_noise(&img);
        denoise(&mut img, &s, 64, 64, 0.0, n);
        let (v1, m1) = var(&img);
        assert!(v1 < v0 * 0.5, "{v0} -> {v1}");
        assert!((m1 - m0).abs() < 0.01);
    }

    /// A uniform grey carrying per-pixel speckle of relative amplitude `amp` around 0.2.
    fn grey_speckle(amp: f32) -> Rgb32f {
        Rgb32f::from_fn(96, 96, |x, y| {
            let n = (((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5) * 2.0 * amp;
            [0.2 * (1.0 + n); 3]
        })
    }

    fn grey_var(im: &Rgb32f) -> f32 {
        let m = im.data.iter().map(|p| p[1]).sum::<f32>() / im.len() as f32;
        im.data.iter().map(|p| (p[1] - m).powi(2)).sum::<f32>() / im.len() as f32
    }

    #[test]
    fn luma_noise_tracks_the_noise_level() {
        // The measured noise the luminance NR sizes itself from must follow the speckle: this is
        // what lets one slider calm a high-ISO frame and leave a low-ISO one nearly alone.
        let lo = luma_noise(&grey_speckle(0.02));
        let hi = luma_noise(&grey_speckle(0.16));
        assert!(hi > 20.0 * lo, "luma noise {lo} -> {hi}");
    }

    #[test]
    fn luminance_nr_at_the_lightroom_setting_is_not_a_no_op() {
        // The Lightroom reference frame renders with Luminance 14; a fixed eps left the coarse
        // luminance grain, so the low setting has to reduce the measured noise materially.
        let mut img = grey_speckle(0.3);
        let v0 = grey_var(&img);
        let mut s = DevelopSettings::default();
        s.detail.nr_luminance = 14.0;
        let n = luma_noise(&img);
        denoise(&mut img, &s, 96, 96, 0.0, n);
        let v1 = grey_var(&img);
        assert!(v1 < 0.8 * v0, "{v0} -> {v1}");
    }

    #[test]
    fn luminance_nr_keeps_a_bright_dark_edge() {
        let mut img = Rgb32f::from_fn(120, 64, |x, y| {
            let base = if x < 60 { 0.10 } else { 0.50 };
            let n = (((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5) * 0.03;
            [base + n; 3]
        });
        let mut s = DevelopSettings::default();
        s.detail.nr_luminance = 14.0;
        let n = luma_noise(&img);
        denoise(&mut img, &s, 120, 64, 0.0, n);
        let mean = |img: &Rgb32f, x0: usize, x1: usize| {
            let (mut s, mut n) = (0.0f32, 0.0f32);
            for y in 0..img.height {
                for x in x0..x1 {
                    s += img.get(x, y)[1];
                    n += 1.0;
                }
            }
            s / n
        };
        let (dark, bright) = (mean(&img, 46, 56), mean(&img, 64, 74));
        assert!(bright - dark > 0.38, "{dark} -> {bright}");
    }
}

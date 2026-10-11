//! Sony ARW lens corrections from the tables in the raw IFD, as the equivalent DNG `OpcodeList3` opcodes, so they
//! reach the same "profile corrections" (`optics.lens_profile`) as a DNG's embedded warp.
//!
//! This is the **modern Sony convention**, shared by the bodies in [`BODIES`]. It was measured on the ILCE-7RM6
//! against the camera JPEG (PR #824: 41 corrected photos, 0.596 px worst at a 1440 px long edge) and confirmed here on
//! the ILCE-7M4 (A7 IV) with the Tamron 35–150 mm F2–2.8 at 92 mm: the same unit, knot spacing, index and
//! frame-filling zoom land the corrected render 0.20 px from the camera JPEG at the crop corner, 0.36 px median over
//! the frame (uncorrected up to 29 px). [`super::arw_lens`] holds the older ILCE-7RM4A convention (unit 16384, the
//! table indexed at the output radius), measured against Sony's Imaging Edge exports; the two differ in unit and
//! index and no single unit fits every lens under either, so they stay separate. A body is listed in [`BODIES`]
//! individually because matching tag numbers and table lengths do not establish the scale, interpolation or crop
//! normalisation on an unmeasured body. See docs/sony-lens-corrections.md.
//!
//! Sony records three count-prefixed signed 16-bit tables in the raw IFD, each with an in-camera on/off tag (ExifTool
//! Sony tag names): distortion [`DISTORTION_PARAMS`] (flag [`DISTORTION_FLAG`]), vignetting [`VIGNETTING_PARAMS`]
//! ([`VIGNETTING_FLAG`]) and lateral chromatic aberration [`CA_PARAMS`] ([`CA_FLAG`]). The first value is the knot
//! count: 16 / 16 / 32 in full-frame files, 11 / 11 / 22 in APS-C crop mode (on the ILCE-7RM6), the rest padding;
//! the CA table holds red's knots, then blue's. The count is read from the file, but only layouts the body was
//! measured with are accepted (the ILCE-7M4: its 16-knot full-frame table; an 11-knot table is left uncorrected until
//! measured; the ILCE-7RM6: 11 and 16).
//!
//! Their meaning was measured by registering blocks of the camera JPEG in each file on LightCraft's uncorrected render
//! (and raw data, for CA) over 54 ILCE-7RM6 photos with four lenses, 41 of them shot with in-camera distortion
//! correction on; nothing else (clean room). The conventions, with their worst residuals at a 1440 px long edge:
//! - knot `i` sits at sensor radius `i · s` from the default-crop centre, `s` = the body's full-frame default-crop
//!   half diagonal / 15 (the same pixel spacing in APS-C mode: the knots are fixed on the sensor); linear in between,
//!   the last segment extended beyond the last knot.
//! - distortion: a corrected radius `r` (px) reads the source radius `S(r) = r · (1 + v(r) / 15000)` (positive:
//!   pincushion), and the camera then zooms (output radius = `z` · corrected radius) so the output fills the default
//!   crop and reads only inside it: the largest `S(ρ/z)/ρ` over the crop border radii `ρ` is 1. 0.596 px over the 34
//!   measurable corrected photos (budget 0.75 px; corners and edge midpoints 0.570 px).
//! - `WarpRectilinear`'s 4-term odd polynomial fits that map within 2.35·10⁻⁴ of the DNG radius (0.220 px) on every
//!   photo, at its 65 fit samples. A table it fits worse than 0.25 px at the samples the crop shows is refused (the
//!   photo stays uncorrected rather than wrongly warped). Between samples the error is somewhat larger: 0.071 against
//!   0.049 px for the full-frame 16–35 mm example in the tests.
//! - vignetting: read but never applied. The camera's own JPEG carries no vignetting correction on these files — its
//!   per-radius ratio to the uncorrected render is flat (1.00 ± 0.02; the camera JPEG is distortion-corrected but not
//!   shading-corrected) — so there is no correction in the reference for a gain model to match. A profile-corrected
//!   export does carry it, with this table's convention (unit 15000), but is not a permissible source. See
//!   docs/sony-lens-corrections.md.
//! - lateral CA: red's (blue's) source radius is `g · (1 + c(g) / 1.6·10⁶)` with `g` the green source radius and `c`
//!   the red (blue) half on the same knots. The unit was refit here against the ILCE-7M4 camera JPEGs: on the corner
//!   fringing of the referenced render, turning the correction on with it removes most of the R/G and B/G CA, and the
//!   best unit scatters 1.4–2.4·10⁶ (median 1.6·10⁶); PR #824's raw-CFA value (3.36·10⁶) under-corrects by about 2×.
//!   The table is parsed and [`warp_opcode`] converts it to the warp's R and B planes for a DNG, but LightCraft does
//!   **not** apply it there: applied after demosaic those planes made the render's CA worse (numbers under
//!   [`lens_opcodes`]). Instead [`ca_opcode`] builds the same table as a red/blue-relative warp over the *raw* radius
//!   and [`crate::opcodes::warp_cfa`] applies it to the CFA colour planes before demosaic, where the red and blue
//!   samples still sit on their own sites. A CA value beyond [`CA_MAX_RELATIVE`] or a colour plane that fails the fit
//!   gate drops the whole correction.
//!
//! The correction is produced whatever the in-camera flag says. Only bodies listed in [`BODIES`] get it: the unit and
//! the knot spacing are verified per body. Another body is added by repeating the measurement on its photos shot with
//! in-camera distortion correction on and listing its full-frame default crop. Method and numbers:
//! docs/sony-lens-corrections.md.

use super::fit_radial_warp;
use crate::opcodes::max_radius;
use crate::{Opcode, Rect};
use lightcraft_tiff::Ifd;
use std::ops::RangeInclusive;

pub(crate) const DISTORTION_PARAMS: u16 = 0x7037;
pub(crate) const VIGNETTING_PARAMS: u16 = 0x7032;
pub(crate) const CA_PARAMS: u16 = 0x7035;
pub(crate) const DISTORTION_FLAG: u16 = 0x7036;
pub(crate) const VIGNETTING_FLAG: u16 = 0x7031;
pub(crate) const CA_FLAG: u16 = 0x7034;

/// A body whose tables use this convention (Exif `Model`): its full-frame default crop (width, height) in px (the
/// knot spacing is its half diagonal / 15) and the distortion-table knot counts its convention was **measured** with.
struct Body {
    model: &'static str,
    full_frame: (f64, f64),
    knots: &'static [usize],
}

/// The bodies using the modern convention, keyed by exact `Model`. ILCE-7RM6: measured in PR #824 (camera JPEG, 54
/// photos; its APS-C crop mode writes 11 knots). ILCE-7M4: measured here (camera JPEG, Tamron 35–150 mm at 92 mm) —
/// only its 16-knot full-frame layout, so an 11-knot table is left uncorrected until it is measured too.
const BODIES: &[Body] = &[
    Body { model: "ILCE-7RM6", full_frame: (9984.0, 6656.0), knots: &[11, 16] },
    Body { model: "ILCE-7M4", full_frame: (7008.0, 4672.0), knots: &[16] },
];

/// The convention of a body whose tables are verified, by exact (trimmed) `Model`.
fn body(model: &str) -> Option<&'static Body> {
    let model = model.trim();
    BODIES.iter().find(|b| b.model == model)
}
/// The distortion table's value of 1 (relative radius); a global least-squares fit over the 34 photos gives 15000.7.
const DISTORTION_UNIT: f64 = 15000.0;
/// The CA table's value of 1 (relative to the green source radius), refit here against the ILCE-7M4 camera JPEGs (the
/// referenced render's corner fringing; see `docs/sony-lens-corrections.md`). The PR #824 value (3.36·10⁶, measured on
/// the ILCE-7RM6's raw CFA) sits above the range those frames measure and under-corrects the render's corner R/G and
/// B/G CA by about 2×; the best unit here scatters 1.4–2.4·10⁶ (median 1.6·10⁶).
const CA_UNIT: f64 = 1.6e6;
/// Largest plausible CA value relative to the green source radius (|c| / [`CA_UNIT`]): about 12 px at the full-frame
/// corner; the largest value in the 54 measured photos' tables is 1724 (FE 24–70 mm), 1724 / 1.6·10⁶ ≈ 1.1·10⁻³.
/// A table beyond it is refused.
const CA_MAX_RELATIVE: f64 = 2e-3;
/// Crop border radii scanned by the frame-filling zoom.
const ZOOM_SCAN: usize = 4096;
/// Largest polynomial fit error accepted at the fit samples the crop shows, px at a 1440 px long edge.
const FIT_ERROR_PX_AT_1440: f64 = 0.25;

/// A file's lens tables: the first `count` values of each (CA: red, blue); `None` when absent or malformed.
/// `vignetting` and `flags` are parsed for the tests and the docs only: don't build an opcode from them (the camera
/// applies no vignetting correction to its JPEGs — measured, see docs/sony-lens-corrections.md — and the flags don't
/// decide anything).
#[derive(Debug)]
// vignetting and the flags never become an opcode
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LensTables {
    pub distortion: Option<Vec<f64>>,
    pub vignetting: Option<Vec<f64>>,
    pub ca: Option<(Vec<f64>, Vec<f64>)>,
    /// In-camera on/off of distortion, vignetting and CA correction, as recorded.
    pub flags: [Option<i64>; 3],
}

/// The values of a count-prefixed integer table whose count is in `counts` and fits the array.
fn table(raw: &Ifd, tag: u16, counts: RangeInclusive<usize>) -> Option<Vec<f64>> {
    let v = raw.value(tag)?;
    let count = usize::try_from(v.get_i64(0)?).ok().filter(|n| counts.contains(n) && *n < v.count())?;
    (1..=count).map(|i| v.get_i64(i).map(|x| x as f64)).collect()
}

/// The lens tables of the raw IFD `raw`.
pub(crate) fn read_tables(raw: &Ifd) -> LensTables {
    let ca = table(raw, CA_PARAMS, 4..=32).filter(|v| v.len().is_multiple_of(2)).map(|v| {
        let (red, blue) = v.split_at(v.len() / 2);
        (red.to_vec(), blue.to_vec())
    });
    LensTables {
        distortion: table(raw, DISTORTION_PARAMS, 2..=16),
        vignetting: table(raw, VIGNETTING_PARAMS, 2..=16),
        ca,
        flags: [raw.i64(DISTORTION_FLAG), raw.i64(VIGNETTING_FLAG), raw.i64(CA_FLAG)],
    }
}

/// The knot spacing (px) of a verified body: its full-frame default-crop half diagonal / 15.
fn knot_spacing(model: &str) -> Option<f64> {
    body(model).map(|b| b.full_frame.0.hypot(b.full_frame.1) / 30.0)
}

/// The knot table at radius `r` px: knot `i` at `i · spacing`, linear in between, the last segment extended beyond
/// the last knot. NaN with fewer than two knots.
fn interpolate(knots: &[f64], spacing: f64, r: f64) -> f64 {
    let t = r / spacing;
    // `as` saturates, so a huge `t` lands on the last segment; NaN fails `t > 0`
    let i = if t > 0.0 { (t as usize).min(knots.len().saturating_sub(2)) } else { 0 };
    match (knots.get(i), knots.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * (t - i as f64),
        _ => f64::NAN,
    }
}

/// The camera's frame-filling zoom `z` for the source radius map `source` and a crop of half size `x × y`: the
/// largest `source(ρ/z)/ρ` over the border radii `ρ` in [min(x, y), hypot(x, y)] is 1, so the output crop reads
/// only inside the source crop and touches it (at the corner for pincushion, the short-edge midpoint for barrel).
/// Bisection over [0.5, 2]; `None` when that range holds no solution or a ratio is not finite.
fn fill_zoom(source: &dyn Fn(f64) -> f64, x: f64, y: f64) -> Option<f64> {
    let (near, far) = (x.min(y), x.hypot(y));
    let overshoots = |z: f64| -> Option<bool> {
        let mut worst = f64::NEG_INFINITY;
        for k in 0..=ZOOM_SCAN {
            let rb = near + k as f64 * (far - near) / ZOOM_SCAN as f64;
            let ratio = source(rb / z) / rb;
            if !ratio.is_finite() {
                return None;
            }
            worst = worst.max(ratio);
        }
        Some(worst > 1.0)
    };
    let (mut lo, mut hi) = (0.5, 2.0);
    if !overshoots(lo)? || overshoots(hi)? {
        return None;
    }
    for _ in 0..100 {
        let z = 0.5 * (lo + hi);
        if overshoots(z)? { lo = z } else { hi = z }
    }
    Some(0.5 * (lo + hi))
}

/// The distortion and CA tables as a `WarpRectilinear` relative to the active area `active` (centred on the default
/// crop `crop`, as the camera corrects), with knots `knot_spacing` px apart: planes R, G, B with `lateral_ca`, or G
/// alone (applied to all three) without it, or when the CA table is absent, implausible or a colour plane's fit
/// fails. `None` when the distortion table is absent or
/// all zero (CA alone is not applied: it is relative to green), the zoom or the G fit fails. A fit fails when it is
/// worse than 0.25 px at a 1440 px long edge at one of the fit samples within the crop (a padded active area, as in
/// lossless files, reaches radii that are never shown).
pub(crate) fn warp_opcode(t: &LensTables, knot_spacing: f64, active: Rect, crop: Rect, lateral_ca: bool) -> Option<Opcode> {
    let v = t.distortion.as_deref().filter(|v| v.iter().any(|&x| x != 0.0))?;
    let (aw, ah) = (active.width as f64, active.height as f64);
    if aw < 2.0 || ah < 2.0 || crop.width == 0 || crop.height == 0 {
        return None;
    }
    let (cw, ch) = (crop.width as f64, crop.height as f64);
    let source = |r: f64| r * (1.0 + interpolate(v, knot_spacing, r) / DISTORTION_UNIT);
    let z = fill_zoom(&source, cw / 2.0, ch / 2.0)?;
    // DNG's normalisation: pixel-index units, radius 1 = the farthest active-area corner
    let (cx, cy) = (crop.x as f64 + (cw - 1.0) / 2.0, crop.y as f64 + (ch - 1.0) / 2.0);
    let m = max_radius(cx, cy, aw - 1.0, ah - 1.0);
    let max_fit_error = FIT_ERROR_PX_AT_1440 * (cw.max(ch) / 1440.0) / m;
    let shown = (cw / 2.0).hypot(ch / 2.0) / m;
    // output radius (px) → green source radius
    let green = |r: f64| source(r / z);
    let plane = |map: &dyn Fn(f64) -> f64| {
        let [k0, k1, k2, k3] = fit_radial_warp(&|u| map(u * m) / m, max_fit_error, shown)?;
        Some([k0, k1, k2, k3, 0.0, 0.0])
    };
    let g = plane(&green)?;
    // red and blue: relative to the green source radius, at that radius
    let colour = |c: &[f64]| {
        plane(&|r| {
            let s = green(r);
            s * (1.0 + interpolate(c, knot_spacing, s) / CA_UNIT)
        })
    };
    // a NaN fails the comparison: refused too
    let plausible = |c: &[f64]| c.iter().all(|v| v.abs() <= CA_MAX_RELATIVE * CA_UNIT);
    let rgb = t.ca.as_ref().filter(|(red, blue)| lateral_ca && plausible(red) && plausible(blue));
    let rgb = rgb.and_then(|(red, blue)| Some(vec![colour(red)?, g, colour(blue)?]));
    Some(Opcode::WarpRectilinear { planes: rgb.unwrap_or_else(|| vec![g]), center: [cx / (aw - 1.0), cy / (ah - 1.0)] })
}

/// The `OpcodeList3` lens corrections of a Sony raw IFD for the body `model` (Exif `Model`); empty for a body whose
/// tables are not verified.
///
/// Distortion only (the G plane): lateral CA is [`ca_opcode`], applied to the CFA before demosaic instead. The warp
/// here runs after demosaic, where demosaicing has already hidden about 70 % of the lens CA (the render shows 0.29 ×
/// the CA measured on the raw CFA) while the warp moves R and B by 0.84 × the table: over the 43 photos with enough
/// corner detail, emitting the CA planes here took the render's corner R/G and B/G CA from rms 0.032 / max 0.125 px
/// (off) to 0.052 / 0.219 px (on) at a 1440 px long edge, and only the 16–35 mm improved.
pub(crate) fn lens_opcodes(raw: &Ifd, model: &str, active: Rect, crop: Rect) -> Vec<Opcode> {
    let Some(b) = body(model) else { return Vec::new() };
    let Some(spacing) = knot_spacing(model) else { return Vec::new() };
    let t = read_tables(raw);
    // only the knot counts this body's convention was measured with; another layout stays uncorrected
    if !t.distortion.as_deref().is_some_and(|v| b.knots.contains(&v.len())) {
        return Vec::new();
    }
    warp_opcode(&t, spacing, active, crop, false).into_iter().collect()
}

/// The body's lateral-CA table as the CFA colour-plane warp that [`crate::opcodes::warp_cfa`] applies *before*
/// demosaic: red's and blue's radial `WarpRectilinear` planes relative to green (green's plane is the identity), on
/// the active area `active` centred on the default crop `crop`. Red's (blue's) source radius at destination radius
/// `r` is `r · (1 + c(r) / [`CA_UNIT`])` with `c` the red (blue) half of the table — the table is indexed at the
/// green source radius (`r` here), because lateral CA is the lens's own sensor-space magnification difference, a
/// property of the raw CFA and not of the corrected frame. `None` for a body whose convention is not verified
/// ([`BODIES`]), when the CA table is absent or a value is implausible ([`CA_MAX_RELATIVE`]), or when either
/// plane's quartic fit over the radii the crop shows is worse than the fit gate.
pub(crate) fn ca_opcode(raw: &Ifd, model: &str, active: Rect, crop: Rect) -> Option<Opcode> {
    let spacing = knot_spacing(model)?;
    ca_planes(&read_tables(raw), spacing, active, crop)
}

/// [`ca_opcode`] on already-read tables.
fn ca_planes(t: &LensTables, knot_spacing: f64, active: Rect, crop: Rect) -> Option<Opcode> {
    let (red, blue) = t.ca.as_ref()?;
    let (aw, ah) = (active.width as f64, active.height as f64);
    if aw < 2.0 || ah < 2.0 || crop.width == 0 || crop.height == 0 {
        return None;
    }
    let (cw, ch) = (crop.width as f64, crop.height as f64);
    let (cx, cy) = (crop.x as f64 + (cw - 1.0) / 2.0, crop.y as f64 + (ch - 1.0) / 2.0);
    let m = max_radius(cx, cy, aw - 1.0, ah - 1.0);
    let max_fit_error = FIT_ERROR_PX_AT_1440 * (cw.max(ch) / 1440.0) / m;
    let shown = (cw / 2.0).hypot(ch / 2.0) / m;
    // a NaN fails the comparison: refused too
    let plausible = |c: &[f64]| c.iter().all(|v| v.abs() <= CA_MAX_RELATIVE * CA_UNIT);
    if !plausible(red) || !plausible(blue) {
        return None;
    }
    let plane = |c: &[f64]| {
        let [k0, k1, k2, k3] = fit_radial_warp(&|u| u * (1.0 + interpolate(c, knot_spacing, u * m) / CA_UNIT), max_fit_error, shown)?;
        Some([k0, k1, k2, k3, 0.0, 0.0])
    };
    Some(Opcode::WarpRectilinear {
        planes: vec![plane(red)?, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], plane(blue)?],
        center: [cx / (aw - 1.0), cy / (ah - 1.0)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{Entry, Value};

    /// The FF 16–35 mm example (corpus `arw-sony-ilce7rm6-ff-land-lossless.arw`, docs/sony-ilce7rm6-lens-corrections.md).
    const FF_DISTORTION: [i16; 17] = [16, 0, 2, 7, 16, 28, 43, 61, 83, 109, 139, 175, 217, 267, 325, 395, 479];
    const FF_VIGNETTING: [i16; 17] = [16, 0, 80, 336, 672, 1120, 1648, 2240, 2880, 3600, 4336, 5104, 5872, 6640, 7392, 8160, 8976];
    const FF_CA: [i16; 33] = [
        32, 242, 234, 224, 212, 196, 176, 154, 132, 108, 86, 66, 46, 30, 16, 8, 4, -276, -258, -238, -212, -184, -148, -114, -74, -40, -6, 20, 42,
        56, 60, 54, 32,
    ];
    /// The CA table of corpus `arw6-sony-ilce7rm6-ff-land-hq.arw` (the same scene; same distortion table).
    const FF_HQ_CA: [i16; 33] = [
        32, 242, 234, 222, 208, 192, 172, 152, 128, 106, 82, 62, 42, 26, 12, 4, 2, -270, -254, -234, -208, -180, -146, -112, -72, -38, -4, 22, 44,
        58, 64, 56, 36,
    ];
    /// The APS-C example (corpus `arw6-sony-ilce7rm6-apsc-land-hq.arw`): counts 11 / 11 / 22, then padding.
    const APSC_DISTORTION: [i16; 17] = [11, 0, -10, -36, -78, -134, -202, -277, -356, -435, -512, -583, -583, -583, -583, -583, -583];
    const APSC_VIGNETTING: [i16; 17] = [11, 0, 48, 208, 496, 880, 1360, 1952, 2624, 3568, 4832, 6176, 0, 0, 0, 0, 0];
    const APSC_CA: [i16; 33] = [
        22, 790, 764, 734, 702, 666, 624, 582, 544, 510, 484, 470, -432, -404, -372, -330, -286, -230, -174, -114, -58, -6, 34, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0,
    ];
    /// The APS-C lossless corpus file `arw-sony-ilce7rm6-apsc-land-lossless.arw` (same scene; its tables differ slightly).
    const APSC_LOSSLESS_DISTORTION: [i16; 17] = [11, 0, -10, -36, -78, -134, -202, -277, -356, -435, -511, -583, -583, -583, -583, -583, -583];
    const APSC_LOSSLESS_CA: [i16; 33] = [
        22, 792, 766, 738, 706, 670, 630, 588, 548, 514, 488, 476, -436, -408, -376, -334, -290, -234, -178, -118, -62, -12, 30, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0,
    ];
    /// ARW6 geometry (the decoder's raw image size and default crop).
    const FF_ACTIVE: Rect = Rect { x: 0, y: 0, width: 10016, height: 6672 };
    const FF_CROP: Rect = Rect { x: 12, y: 8, width: 9984, height: 6656 };
    const APSC_ACTIVE: Rect = Rect { x: 0, y: 0, width: 6592, height: 4372 };
    const APSC_CROP: Rect = Rect { x: 16, y: 10, width: 6528, height: 4352 };
    /// The G planes of the examples on their ARW6 geometry, and the R and B planes with each file's CA table
    /// (docs/sony-ilce7rm6-lens-corrections.md).
    const FF_G: [f64; 4] = [0.9714495823, 0.022508032, 0.0006090085237, 0.005683284548];
    const FF_HQ_R: [f64; 4] = [0.9715876164975256, 0.022202758545419768, 0.0008127378264317234, 0.005647704278542618];
    const FF_HQ_B: [f64; 4] = [0.9713023255794672, 0.023058508973172197, 0.00013440796434607962, 0.00577681025234552];
    const APSC_G: [f64; 4] = [1.01627551, -0.0618606692, 0.02768349621, -0.005595798468];
    const APSC_R: [f64; 4] = [1.0167553210154523, -0.06228892710454764, 0.028022155430202787, -0.005697269843348392];
    const APSC_B: [f64; 4] = [1.016023914058575, -0.061365294058996905, 0.027367776026911886, -0.00550994507898096];

    fn knots() -> f64 {
        knot_spacing("ILCE-7RM6").unwrap()
    }

    fn ifd(tags: Vec<(u16, Value)>) -> Ifd {
        let mut entries: Vec<Entry> = tags.into_iter().map(|(tag, value)| Entry { tag, value, offset: 0 }).collect();
        entries.sort_by_key(|e| e.tag);
        Ifd { entries, ..Default::default() }
    }

    fn sony_lens_ifd(distortion: &[i16], vignetting: &[i16], ca: &[i16], flags: [u16; 3]) -> Ifd {
        ifd(vec![
            (DISTORTION_PARAMS, Value::SShort(distortion.to_vec())),
            (VIGNETTING_PARAMS, Value::SShort(vignetting.to_vec())),
            (CA_PARAMS, Value::SShort(ca.to_vec())),
            (DISTORTION_FLAG, Value::Short(vec![flags[0]])),
            (VIGNETTING_FLAG, Value::Short(vec![flags[1]])),
            (CA_FLAG, Value::Short(vec![flags[2]])),
        ])
    }

    fn tables_with_distortion(v: &[f64]) -> LensTables {
        LensTables { distortion: Some(v.to_vec()), vignetting: None, ca: None, flags: [None; 3] }
    }

    fn values(table: &[i16]) -> Vec<f64> {
        table[1..=table[0] as usize].iter().map(|&v| v as f64).collect()
    }

    /// The warp's planes and centre against the recipe's numbers (docs/sony-ilce7rm6-lens-corrections.md); returns the planes.
    fn assert_warp(op: Option<Opcode>, center: [f64; 2], k: &[[f64; 4]]) -> Vec<[f64; 6]> {
        let Some(Opcode::WarpRectilinear { planes, center: c }) = op else { panic!("no warp: {op:?}") };
        assert_eq!(planes.len(), k.len());
        assert!((c[0] - center[0]).abs() < 1e-9 && (c[1] - center[1]).abs() < 1e-9, "centre {c:?}, want {center:?}");
        for (p, k) in planes.iter().zip(k) {
            for (i, want) in k.iter().enumerate() {
                assert!((p[i] - want).abs() < 1e-6, "k{i} {}, want {want} ({p:?})", p[i]);
            }
            assert_eq!((p[4], p[5]), (0.0, 0.0), "radial only");
        }
        planes
    }

    #[test]
    fn reads_the_three_tables_and_flags() {
        let t = read_tables(&sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_CA, [0, 2, 1]));
        let d = t.distortion.unwrap();
        assert_eq!((d.len(), d[0], d[15]), (16, 0.0, 479.0));
        let v = t.vignetting.unwrap();
        assert_eq!((v.len(), v[1], v[15]), (16, 80.0, 8976.0));
        let (red, blue) = t.ca.unwrap();
        assert_eq!((red.len(), red[0], red[15]), (16, 242.0, 4.0));
        assert_eq!((blue.len(), blue[0], blue[15]), (16, -276.0, 32.0));
        assert_eq!(t.flags, [Some(0), Some(2), Some(1)]);
    }

    #[test]
    fn apsc_tables_use_only_the_first_count_values() {
        let t = read_tables(&sony_lens_ifd(&APSC_DISTORTION, &APSC_VIGNETTING, &APSC_CA, [0, 2, 1]));
        let d = t.distortion.unwrap();
        assert_eq!((d.len(), d[10]), (11, -583.0));
        assert_eq!(t.vignetting.unwrap().len(), 11);
        let (red, blue) = t.ca.unwrap();
        assert_eq!((red.len(), red[10]), (11, 470.0));
        assert_eq!((blue.len(), blue[0], blue[10]), (11, -432.0, 34.0));
    }

    #[test]
    fn missing_or_malformed_tables_yield_no_opcode() {
        let mut short = FF_DISTORTION.to_vec();
        short.pop();
        let mut count_17 = FF_DISTORTION.to_vec();
        count_17[0] = 17;
        count_17.push(500);
        let mut too_large = FF_DISTORTION.to_vec();
        too_large[0] = 600;
        for (what, table) in [
            ("count 15 with 3 values", vec![15, 0, 2, 7]),
            ("count 17", count_17),
            ("empty", vec![]),
            ("count 16 with 15 values", short),
            ("count 1", vec![1, 5]),
            ("count 0", vec![0, 5, 6]),
            ("negative count", vec![-16, 0, 2, 7]),
            ("count larger than the array", too_large),
        ] {
            let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(table))]);
            let t = read_tables(&ifd);
            assert!(t.distortion.is_none(), "{what}");
            assert!(warp_opcode(&t, knots(), FF_ACTIVE, FF_CROP, false).is_none(), "{what}");
            assert!(lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_empty(), "{what}");
        }
        // not an integer array, or absent
        for value in [Value::Ascii("16 0 2 7".into()), Value::Double(vec![2.0, 0.0, 1.0])] {
            assert!(read_tables(&ifd(vec![(DISTORTION_PARAMS, value)])).distortion.is_none());
        }
        let none = read_tables(&ifd(vec![]));
        assert!(none.distortion.is_none() && none.vignetting.is_none() && none.ca.is_none() && none.flags == [None; 3]);
        // the CA table needs an even count of 4..=32
        for ca in [vec![3, 1, 2, 3], vec![34; 35], vec![2, 1, 2]] {
            assert!(read_tables(&ifd(vec![(CA_PARAMS, Value::SShort(ca))])).ca.is_none());
        }
    }

    #[test]
    fn hostile_tables_are_rejected() {
        let folded = [0.0, 2000.0, 1000.0, 3000.0, 2000.0, 4000.0, 3000.0, 5000.0, 4000.0, 6000.0, 5000.0, 7000.0, 6000.0, 8000.0, 7000.0, 9000.0];
        for (what, table) in [
            ("huge", vec![(i64::MAX / 4) as f64; 16]),
            ("most negative", vec![i64::MIN as f64; 16]),
            ("zig-zag", folded.to_vec()),
            ("all zero", vec![0.0; 16]),
            ("one knot", vec![100.0]),
            ("no knots", vec![]),
            ("not finite", vec![0.0, f64::NAN, 20.0]),
            ("infinite", vec![0.0, f64::INFINITY, 20.0]),
        ] {
            assert!(warp_opcode(&tables_with_distortion(&table), knots(), FF_ACTIVE, FF_CROP, false).is_none(), "{what}");
        }
        // the same through the IFD, as 64-bit values
        for v in [i64::MAX / 4, i64::MIN, i64::MAX] {
            let mut table = vec![v; 17];
            table[0] = 16;
            let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SLong8(table))]);
            assert!(lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_empty(), "{v}");
        }
        // degenerate geometry
        let ff = tables_with_distortion(&values(&FF_DISTORTION));
        for (what, knots, active, crop) in [
            ("empty crop", knots(), FF_ACTIVE, Rect::new(12, 8, 0, 6656)),
            ("1 × 1 active area", knots(), Rect::new(0, 0, 1, 1), Rect::new(0, 0, 1, 1)),
            ("knot spacing NaN", f64::NAN, FF_ACTIVE, FF_CROP),
            ("knot spacing 0", 0.0, FF_ACTIVE, FF_CROP),
        ] {
            assert!(warp_opcode(&ff, knots, active, crop, false).is_none(), "{what}");
        }
    }

    #[test]
    fn ff_example_table_becomes_the_measured_warp() {
        let t = tables_with_distortion(&values(&FF_DISTORTION));
        assert_warp(warp_opcode(&t, knots(), FF_ACTIVE, FF_CROP, false), [0.4996005991, 0.5], &[FF_G]);
    }

    #[test]
    fn apsc_example_table_becomes_the_measured_warp() {
        let t = tables_with_distortion(&values(&APSC_DISTORTION));
        let op = warp_opcode(&t, knots(), APSC_ACTIVE, APSC_CROP, false);
        assert_warp(op, [0.4975724473, 0.5], &[APSC_G]);
    }

    #[test]
    fn ca_table_shifts_red_and_blue_planes() {
        let t = read_tables(&sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_HQ_CA, [0, 2, 1]));
        let planes = assert_warp(warp_opcode(&t, knots(), FF_ACTIVE, FF_CROP, true), [0.4996005991, 0.5], &[FF_HQ_R, FF_G, FF_HQ_B]);
        assert_ne!(planes[0][0], planes[1][0]);
        // G is the distortion plane alone
        let g = warp_opcode(&tables_with_distortion(&values(&FF_DISTORTION)), knots(), FF_ACTIVE, FF_CROP, false);
        assert!(matches!(g, Some(Opcode::WarpRectilinear { planes: ref p, .. }) if p == &[planes[1]]), "{g:?}");
    }

    #[test]
    fn apsc_ca_planes_match_the_measured_warp() {
        let t = read_tables(&sony_lens_ifd(&APSC_DISTORTION, &APSC_VIGNETTING, &APSC_CA, [0, 2, 1]));
        assert_warp(warp_opcode(&t, knots(), APSC_ACTIVE, APSC_CROP, true), [0.4975724473, 0.5], &[APSC_R, APSC_G, APSC_B]);
    }

    #[test]
    fn ca_without_distortion_is_not_applied() {
        let ifd = ifd(vec![(CA_PARAMS, Value::SShort(FF_HQ_CA.to_vec()))]);
        let t = read_tables(&ifd);
        assert!(t.ca.is_some() && t.distortion.is_none());
        assert!(warp_opcode(&t, knots(), FF_ACTIVE, FF_CROP, true).is_none());
        assert!(lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_empty());
        // an all-zero distortion table counts as absent
        let zero = LensTables { ca: t.ca, ..tables_with_distortion(&[0.0; 16]) };
        assert!(warp_opcode(&zero, knots(), FF_ACTIVE, FF_CROP, true).is_none());
    }

    #[test]
    fn rejected_ca_keeps_the_single_green_plane() {
        let green = assert_warp(
            warp_opcode(&tables_with_distortion(&values(&FF_DISTORTION)), knots(), FF_ACTIVE, FF_CROP, false),
            [0.4996005991, 0.5],
            &[FF_G],
        );
        let ok = values(&FF_HQ_CA);
        let huge = vec![(i64::MAX / 4) as f64; 16];
        for (what, red, blue) in [
            ("huge red", huge.clone(), ok[16..].to_vec()),
            ("huge blue", ok[..16].to_vec(), huge),
            ("most negative blue", ok[..16].to_vec(), vec![i64::MIN as f64; 16]),
            ("not finite red", vec![0.0, f64::NAN, 20.0], ok[16..].to_vec()),
            ("one knot", vec![100.0], vec![100.0]),
        ] {
            let t = LensTables { ca: Some((red, blue)), ..tables_with_distortion(&values(&FF_DISTORTION)) };
            let Some(Opcode::WarpRectilinear { planes, .. }) = warp_opcode(&t, knots(), FF_ACTIVE, FF_CROP, true) else { panic!("{what}: no warp") };
            assert_eq!(planes, green, "{what}");
        }
        // through the IFD: an odd count, or values that overflow the fit
        let mut odd = FF_HQ_CA.to_vec();
        odd[0] = 31;
        let mut huge_ifd = vec![i64::MAX / 4; 33];
        huge_ifd[0] = 32;
        // a constant 32767 (the SShort maximum) fits as well as G does and would shift red ~28 px at the full-res
        // corner: refused as implausible (bound 2·10⁻³ relative = 3200)
        let mut max = vec![32767; 33];
        max[0] = 32;
        for (what, ca) in [("odd count", Value::SShort(odd)), ("huge", Value::SLong8(huge_ifd)), ("constant 32767", Value::SShort(max))] {
            let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(FF_DISTORTION.to_vec())), (CA_PARAMS, ca)]);
            let op = warp_opcode(&read_tables(&ifd), knots(), FF_ACTIVE, FF_CROP, true);
            assert!(matches!(op, Some(Opcode::WarpRectilinear { ref planes, .. }) if planes == &green), "{what}: {op:?}");
        }
        // values up to the bound are kept
        let mut at_bound = vec![3200; 33];
        at_bound[0] = 32;
        let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(FF_DISTORTION.to_vec())), (CA_PARAMS, Value::SShort(at_bound))]);
        let op = warp_opcode(&read_tables(&ifd), knots(), FF_ACTIVE, FF_CROP, true);
        assert!(matches!(op, Some(Opcode::WarpRectilinear { ref planes, .. }) if planes.len() == 3), "{op:?}");
    }

    /// The default: a valid CA table is read and converts (above), but `lens_opcodes` emits the G plane alone.
    #[test]
    fn lens_opcodes_emit_the_green_plane_only() {
        let ifd = sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_HQ_CA, [1, 2, 1]);
        let with_ca = warp_opcode(&read_tables(&ifd), knots(), FF_ACTIVE, FF_CROP, true);
        assert!(matches!(with_ca, Some(Opcode::WarpRectilinear { ref planes, .. }) if planes.len() == 3), "{with_ca:?}");
        let ops = lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP);
        let [Opcode::WarpRectilinear { planes, .. }] = ops.as_slice() else { panic!("{ops:?}") };
        assert_eq!(planes.len(), 1);
        let green = warp_opcode(&tables_with_distortion(&values(&FF_DISTORTION)), knots(), FF_ACTIVE, FF_CROP, false);
        assert_eq!(ops.first(), green.as_ref());
    }

    /// The corpus lossless files' own tables on their decoded geometry (padded active areas; the corpus test checks
    /// the G plane from the files): R, G, B with the CA planes enabled.
    #[test]
    fn lossless_examples_ca_planes_match_the_measured_warp() {
        let ff = read_tables(&sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_CA, [0, 2, 1]));
        let ff_active = Rect { x: 0, y: 0, width: 10240, height: 7168 };
        assert_warp(
            warp_opcode(&ff, knots(), ff_active, FF_CROP, true),
            [0.488670768629749, 0.4653969582810102],
            &[
                [0.9718444289020475, 0.02288026728389749, 0.008959452753525485, 0.002998298318920514],
                [0.9717033132764838, 0.023249297636470764, 0.008638979168983145, 0.0030910418313146253],
                [0.9715520430090133, 0.023915976088559294, 0.007930691597653732, 0.0032840026385809497],
            ],
        );
        let apsc = read_tables(&sony_lens_ifd(&APSC_LOSSLESS_DISTORTION, &APSC_VIGNETTING, &APSC_LOSSLESS_CA, [0, 2, 1]));
        let apsc_active = Rect { x: 0, y: 0, width: 6656, height: 4608 };
        assert_warp(
            warp_opcode(&apsc, knots(), apsc_active, APSC_CROP, true),
            [0.49278737791134486, 0.4743868026915563],
            &[
                [1.016848036381034, -0.06933393478273298, 0.03651761973488345, -0.009539665806630427],
                [1.016366854640654, -0.06888241897861273, 0.0361574525336676, -0.009438825010926135],
                [1.0161124860429585, -0.06833671509699642, 0.03577270292577328, -0.009324018253954285],
            ],
        );
    }

    /// The lossless files' padded active area (10240 × 7168) puts the farthest active-area corner (DNG radius 1) well
    /// beyond the crop's (u 0.925): the gate looks only at the radii the crop shows. The FF example × 1.5 fits within
    /// 0.12 px at 1440 at the samples there, 0.27 px at all 65.
    #[test]
    fn padded_active_area_gates_only_the_displayed_radii() {
        let strong: Vec<f64> = values(&FF_DISTORTION).iter().map(|v| v * 1.5).collect();
        let padded = Rect { x: 0, y: 0, width: 10240, height: 7168 };
        let op = warp_opcode(&tables_with_distortion(&strong), knots(), padded, FF_CROP, false);
        assert!(matches!(op, Some(Opcode::WarpRectilinear { .. })), "{op:?}");
    }

    #[test]
    fn unknown_model_gets_no_opcodes() {
        let ifd = sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_CA, [0, 2, 1]);
        for unverified in ["ILCE-7M3", "ILCE-7RM4", "ILCE-7M4A", ""] {
            assert!(lens_opcodes(&ifd, unverified, FF_ACTIVE, FF_CROP).is_empty(), "{unverified}");
        }
        // the camera's in-camera flag (0 = off here) doesn't matter: the table is applied
        let ops = lens_opcodes(&ifd, " ILCE-7RM6 ", FF_ACTIVE, FF_CROP);
        assert_eq!(ops, warp_opcode(&read_tables(&ifd), knots(), FF_ACTIVE, FF_CROP, false).into_iter().collect::<Vec<_>>());
        assert_eq!(ops.len(), 1);
    }

    /// The ILCE-7M4 (A7 IV) table measured here: the ARW `DSC03836.ARW` (Tamron 35–150 mm F2–2.8 at 92 mm, in-camera
    /// distortion correction Auto), raw-IFD `0x7037`. Same convention as the ILCE-7RM6 (unit 15000, knots
    /// `corner/15`): the warp is centred on the default crop, one G plane, the coefficients fitted against the camera
    /// JPEG (0.20 px at the corner of a 1440 px crop, 0.36 px median over the frame).
    const M4_DISTORTION: [i16; 17] = [16, -4, 0, 11, 27, 49, 75, 110, 149, 198, 253, 318, 392, 476, 571, 681, 807];
    const M4_ACTIVE: Rect = Rect { x: 0, y: 0, width: 7168, height: 5120 };
    const M4_CROP: Rect = Rect { x: 12, y: 8, width: 7008, height: 4672 };
    const M4_G: [f64; 4] = [0.9543647459, 0.0441280853, 0.0107628984, 0.0011704438];

    #[test]
    fn ilce_7m4_table_becomes_the_measured_warp() {
        let spacing = knot_spacing("ILCE-7M4").unwrap();
        // the body's knot spacing is its full-frame default-crop half diagonal / 15
        assert!((spacing - (7008f64.hypot(4672.0) / 30.0)).abs() < 1e-9);
        let t = tables_with_distortion(&values(&M4_DISTORTION));
        let op = warp_opcode(&t, spacing, M4_ACTIVE, M4_CROP, false);
        let planes = assert_warp(op, [3515.5 / 7167.0, 2343.5 / 5119.0], &[M4_G]);
        assert_eq!(planes.len(), 1);
        // through the IFD and `lens_opcodes`, the same opcode (distortion only; CA off)
        let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(M4_DISTORTION.to_vec()))]);
        let ops = lens_opcodes(&ifd, "ILCE-7M4", M4_ACTIVE, M4_CROP);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops.first(), warp_opcode(&read_tables(&ifd), spacing, M4_ACTIVE, M4_CROP, false).as_ref());
    }

    /// Only the table layouts a body was measured with apply: the ILCE-7M4's 16-knot full-frame table, not an
    /// 11-knot one (an unmeasured layout), which the ILCE-7RM6's convention does accept (its APS-C crop mode).
    #[test]
    fn only_the_measured_knot_counts_apply() {
        let mut eleven = M4_DISTORTION.to_vec();
        eleven[0] = 11;
        let ifd11 = ifd(vec![(DISTORTION_PARAMS, Value::SShort(eleven))]);
        assert!(lens_opcodes(&ifd11, "ILCE-7M4", M4_ACTIVE, M4_CROP).is_empty());
        assert_eq!(lens_opcodes(&ifd11, "ILCE-7RM6", FF_ACTIVE, FF_CROP).len(), 1);
    }

    /// The ILCE-7M4's lateral-CA table (red's 16 knots, then blue's) parses and converts to R, G, B planes — its
    /// values are well inside the plausibility bound — but `lens_opcodes` emits the G plane alone: off by default.
    #[test]
    fn ilce_7m4_ca_planes_convert_but_stay_off() {
        const CA: [i16; 33] = [
            32, 0, -128, -128, -256, -256, -384, -384, -384, -384, -384, -512, -512, -512, -384, -384, -256, -128, 0, 128, 128, 128, 128, 256, 256,
            256, 256, 256, 256, 256, 256, 256, 128,
        ];
        let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(M4_DISTORTION.to_vec())), (CA_PARAMS, Value::SShort(CA.to_vec()))]);
        let t = read_tables(&ifd);
        let (red, blue) = t.ca.as_ref().expect("CA parsed");
        assert_eq!((red.len(), blue.len()), (16, 16));
        let with_ca = warp_opcode(&t, knot_spacing("ILCE-7M4").unwrap(), M4_ACTIVE, M4_CROP, true);
        let Some(Opcode::WarpRectilinear { planes, .. }) = with_ca else { panic!("no warp") };
        assert_eq!(planes.len(), 3);
        assert_ne!(planes[0], planes[1], "red differs from green");
        let ops = lens_opcodes(&ifd, "ILCE-7M4", M4_ACTIVE, M4_CROP);
        let [Opcode::WarpRectilinear { planes, .. }] = ops.as_slice() else { panic!("{ops:?}") };
        assert_eq!(planes.len(), 1, "the CA planes are off by default");
    }

    /// `ca_opcode` builds the red and blue planes relative to green (green's plane is the identity) on the same
    /// centre as the distortion warp, and each reproduces `r·(1 + c(r)/CA_UNIT)` within the fit gate.
    #[test]
    fn ca_opcode_is_red_and_blue_relative_to_green() {
        let t = read_tables(&sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_HQ_CA, [0, 2, 1]));
        let Some(Opcode::WarpRectilinear { planes, center }) = ca_planes(&t, knots(), FF_ACTIVE, FF_CROP) else { panic!("no CA warp") };
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[1], [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], "green is the identity");
        assert!((center[0] - 0.4996005991).abs() < 1e-9 && (center[1] - 0.5).abs() < 1e-9, "the same centre as the distortion warp: {center:?}");
        let (cx, cy) = (FF_CROP.x as f64 + (FF_CROP.width as f64 - 1.0) / 2.0, FF_CROP.y as f64 + (FF_CROP.height as f64 - 1.0) / 2.0);
        let m = max_radius(cx, cy, FF_ACTIVE.width as f64 - 1.0, FF_ACTIVE.height as f64 - 1.0);
        let c = values(&FF_HQ_CA);
        let (red, blue) = (&c[..16], &c[16..]);
        let eval = |p: &[f64; 6], u: f64| u * (p[0] + u * u * (p[1] + u * u * (p[2] + u * u * p[3])));
        for u in [0.2, 0.5, 0.8, 0.95] {
            let r = u * m;
            assert!((eval(&planes[0], u) - u * (1.0 + interpolate(red, knots(), r) / CA_UNIT)).abs() < 1e-3, "red at u={u}");
            assert!((eval(&planes[2], u) - u * (1.0 + interpolate(blue, knots(), r) / CA_UNIT)).abs() < 1e-3, "blue at u={u}");
        }
        assert_ne!(planes[0], planes[2], "the table is not symmetric");
    }

    /// `ca_opcode` returns nothing for an unverified body, an absent table, or an implausible value.
    #[test]
    fn ca_opcode_refuses_unverified_or_implausible() {
        let full = sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_HQ_CA, [0, 2, 1]);
        assert!(ca_opcode(&full, "ILCE-7M3", FF_ACTIVE, FF_CROP).is_none(), "unverified body");
        let no_ca = ifd(vec![(DISTORTION_PARAMS, Value::SShort(FF_DISTORTION.to_vec()))]);
        assert!(ca_opcode(&no_ca, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_none(), "no CA table");
        // the SShort maximum is beyond CA_MAX_RELATIVE (2·10⁻³)
        let mut max = vec![32767i16; 33];
        max[0] = 32;
        let big = ifd(vec![(CA_PARAMS, Value::SShort(max))]);
        assert!(ca_opcode(&big, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_none(), "implausible value");
    }

    /// The ILCE-7M4's CA table becomes a CFA colour-plane warp on that body's geometry: three planes, an identity
    /// green, and red and blue shifted in opposite directions at the corner (the table's signs).
    #[test]
    fn ilce_7m4_ca_becomes_a_cfa_warp() {
        const CA: [i16; 33] = [
            32, 0, -128, -128, -256, -256, -384, -384, -384, -384, -384, -512, -512, -512, -384, -384, -256, -128, 0, 128, 128, 128, 128, 256, 256,
            256, 256, 256, 256, 256, 256, 256, 128,
        ];
        let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(M4_DISTORTION.to_vec())), (CA_PARAMS, Value::SShort(CA.to_vec()))]);
        let Some(Opcode::WarpRectilinear { planes, .. }) = ca_opcode(&ifd, "ILCE-7M4", M4_ACTIVE, M4_CROP) else { panic!("no CA warp") };
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[1], [1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        // the table's first knots are negative (red in) then positive (blue out): the radial factors differ
        assert!(planes[0][0] < 1.0 && planes[2][0] > 1.0, "red in, blue out: {planes:?}");
    }
}

//! Film-negative conversion for LightCraft — a clean-room implementation of the
//! behaviour measured from Negative Lab Pro's conversion.
//!
//! The **confirmed** maths (the reference run, 8-bit floor agreement on a
//! neutral grey wedge) is the per-channel linear inverting window
//! `out = clamp01((base - in) / (base - dark))`, where `base` is the film base
//! measured from the frame border and `dark` is the negative's dark endpoint.
//! [`convert`] chains it with the classic preset vocabulary and the public DNG
//! Camera Profile chain.
//!
//! Two gaps are carried forward honestly (they are `nlp-re` `KNOWN-GAPS.md`
//! entries, not solved here):
//!
//! - **The dark endpoint `dark` is not fitted.** The rule the real plugin uses
//!   to derive it from the image is unknown and content-dependent, so it is an
//!   explicit, serializable parameter of [`Params`] with no invented formula.
//! - **The output has a nonlinear colour-mixing component** on top of the
//!   per-channel curve; [`apply_preset`] models the preset vocabulary linearly
//!   and does not reproduce it.
//!
//! LightCraft ships **none** of the plugin's data (no presets, no `.dcp`
//! profiles, no LUTs). A camera profile is read from a path the user supplies
//! (see [`profiles::get`]); when it is absent the profile half is skipped.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod color;
pub mod convert;
pub mod dcp;
pub mod looktable;
pub mod params;
pub mod profiles;
pub mod tonecurve;

pub use color::{CalibrationIlluminant, CameraColorSpec, Mat3, Matrix};
pub use convert::{
    BASE_MIN_SPREAD, BASE_MIN_WARM_FRACTION, BASE_QUANTILE, BASE_WARM_MIN, BASE_WARM_SPREAD, Image, apply_preset, apply_profile, convert,
    detect_base_border, encode, invert, linearize, measure_base, measure_base_auto, measure_dark,
    neutralise,
};
pub use dcp::{Dcp, DcpError, Generation};
pub use looktable::{HsbModify, HueSatEncoding, HueSatMap, HueSatTable, HueSatTableData, LookTable, LookTableData, LookTableError};
pub use params::{ColorDensity, Lut, Params, PresetParams, ProcessOrder, ToneProfile, WorkingSpace};
pub use profiles::{ProfileFile, default_dirs, discover, discover_default, find_for_models};
pub use tonecurve::{ToneCurve, ToneCurveError};

use lightcraft_raster::Rgb32f;

/// The dark endpoint of the invert window. It is a **content-dependent statistic of the analysed
/// frame**: the per-channel robust near-min (`nlp-re` notes 07/08: `whitePointOrigin` is the
/// across-roll *minimum*, min-like; the one fully-known real frame has endpoint 10 against a frame
/// minimum of 0). [`convert::measure_dark`] implements it. This constant is the **placeholder** the
/// LightCraft controls default to — the grey-wedge fit, in encoded `0..1` — kept so a bare settings
/// file reproduces the documented reference run; the conversion commands switch to the measured
/// endpoint (`Params::dark_auto`). It is documented as a placeholder, not discovered behaviour.
pub const PLACEHOLDER_DARK: [f32; 3] = [7.17 / 255.0, 1.56 / 255.0, -0.89 / 255.0];

/// The default `borderBuffer` setting: the frame picks the film-base ring itself (see
/// [`detect_base_border`]). Any value `>= 0` is an explicit inset fraction.
pub const DEFAULT_BORDER: f32 = -1.0;

/// A conversion [`Image`] from a LightCraft RGB buffer.
pub fn image_from_rgb(src: &Rgb32f) -> Image {
    Image { width: src.width, height: src.height, pixels: src.data.clone() }
}

/// A LightCraft RGB buffer from a conversion [`Image`].
pub fn image_to_rgb(img: &Image) -> Rgb32f {
    Rgb32f { width: img.width, height: img.height, data: img.pixels.clone() }
}

/// Run the conversion on an encoded-RGB buffer (the scan's own `0..1` values).
pub fn convert_rgb(src: &Rgb32f, p: &Params) -> Rgb32f {
    image_to_rgb(&convert(&image_from_rgb(src), p))
}

/// Measure the film base from an encoded-RGB buffer's border ring.
pub fn measure_base_rgb(src: &Rgb32f, border: f32) -> [f32; 3] {
    measure_base(&image_from_rgb(src), border)
}

/// Linear Rec.2020 (LightCraft's working space, D65) → encoded sRGB, the space
/// the confirmed invert window acts in.
pub fn linear_rec2020_to_srgb_encoded(v: [f32; 3]) -> [f32; 3] {
    let m = rec2020_to_srgb();
    let lin = [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ];
    lin.map(lightcraft_color::transfer::linear_to_srgb)
}

/// Inverse of [`linear_rec2020_to_srgb_encoded`].
pub fn srgb_encoded_to_linear_rec2020(v: [f32; 3]) -> [f32; 3] {
    let lin = v.map(lightcraft_color::transfer::srgb_to_linear);
    let m = srgb_to_rec2020();
    [
        m[0][0] * lin[0] + m[0][1] * lin[1] + m[0][2] * lin[2],
        m[1][0] * lin[0] + m[1][1] * lin[1] + m[1][2] * lin[2],
        m[2][0] * lin[0] + m[2][1] * lin[1] + m[2][2] * lin[2],
    ]
}

fn mat3_f32(m: lightcraft_color::Mat3) -> [[f32; 3]; 3] {
    m.to_f32()
}

fn rec2020_to_srgb() -> [[f32; 3]; 3] {
    mat3_f32(lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB))
}

fn srgb_to_rec2020() -> [[f32; 3]; 3] {
    mat3_f32(lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020))
}

/// Apply the conversion to a LightCraft scene-linear Rec.2020 buffer in place:
/// round-trip to encoded sRGB (the scan's space), run [`convert`], and back to
/// scene-linear Rec.2020 so the rest of the pipeline is unchanged.
///
/// The source is a rendered file (a negative scan), so its own encoding is sRGB;
/// the round trip reconstructs the scan's values the window was measured on.
pub fn apply_linear_rec2020(src: &mut Rgb32f, p: &Params) {
    let mut encoded = src.clone();
    for px in &mut encoded.data {
        *px = linear_rec2020_to_srgb_encoded(*px);
    }
    let mut params = p.clone();
    if params.dark_auto {
        // Measure on the encoded frame the window inverts (post-white-balance, pre-profile): the same
        // frame `invert` sees, so the endpoint is self-consistent with the window. The plugin's own
        // endpoint is a statistic of the *profiled* frame; the residual is the profile stage order
        // (see Gaps 4).
        params.dark = measure_dark(&image_from_rgb(&encoded));
    }
    let out = convert(&image_from_rgb(&encoded), &params);
    src.width = out.width;
    src.height = out.height;
    src.data = out.pixels;
    for px in &mut src.data {
        *px = srgb_encoded_to_linear_rec2020(*px);
    }
}

/// Measure the film base from a scene-linear Rec.2020 buffer: round-trip to
/// encoded sRGB (the space the base was measured on) and take the border high
/// quantile. `border < 0` asks the frame to pick the film-base ring.
pub fn measure_base_linear_rec2020(src: &Rgb32f, border: f32) -> [f32; 3] {
    measure_base_linear_rec2020_full(src, border).0
}

/// [`measure_base_linear_rec2020`], also returning the inset the base was measured at (`None` when
/// the frame showed no film-base ring and its outermost ring was kept).
pub fn measure_base_linear_rec2020_full(src: &Rgb32f, border: f32) -> ([f32; 3], Option<f32>) {
    let enc = src.map(linear_rec2020_to_srgb_encoded);
    let img = image_from_rgb(&enc);
    if border < 0.0 { measure_base_auto(&img) } else { (measure_base(&img, border), Some(border)) }
}

/// Measure the dark endpoint from a scene-linear Rec.2020 buffer: round-trip to encoded sRGB (the
/// space the invert window acts in) and take the frame's per-channel robust near-min
/// ([`convert::measure_dark`]). This is a *preview* of what the pipeline will measure on the
/// white-balanced analysed frame; the pipeline measures its own, authoritative value.
pub fn measure_dark_linear_rec2020(src: &Rgb32f) -> [f32; 3] {
    let enc = src.map(linear_rec2020_to_srgb_encoded);
    measure_dark(&image_from_rgb(&enc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_rec2020_roundtrip() {
        for &v in &[[0.0f32, 0.0, 0.0], [0.5, 0.5, 0.5], [0.2, 0.6, 0.9], [1.0, 1.0, 1.0]] {
            let back = srgb_encoded_to_linear_rec2020(linear_rec2020_to_srgb_encoded(v));
            for c in 0..3 {
                assert!((back[c] - v[c]).abs() < 1e-4, "{v:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn convert_on_a_grey_inverts_toward_white() {
        // A grey set to the base reads as 0 (black positive); a darker grey reads
        // brighter.
        let p = Params { base: [0.84, 0.66, 0.47], dark: PLACEHOLDER_DARK, ..Params::default() };
        let base_img = Rgb32f::filled(2, 1, [0.84, 0.66, 0.47]);
        let out = convert_rgb(&base_img, &p);
        assert!(out.data.iter().all(|v| v.iter().all(|c| c.abs() < 1e-6)), "{:?}", out.data);
    }

    #[test]
    fn dark_auto_maps_the_frame_near_min_to_white() {
        // `dark_auto` reads the endpoint from the analysed frame; the near-min pixel must then map
        // to (near) white, which the placeholder dark (≈0) does not do for a frame whose dark end is
        // much above zero.
        let base_enc = [0.84f32, 0.66, 0.47];
        let dark_enc = [0.30f32, 0.24, 0.18];
        let mid_enc = [0.60f32, 0.50, 0.40];
        let lin = srgb_encoded_to_linear_rec2020;
        let (w, h) = (40usize, 40usize);
        let mut img = Rgb32f::filled(w, h, lin(mid_enc));
        for y in 0..h {
            for x in 0..4 {
                img.set(x, y, lin(base_enc));
            }
            for x in 8..12 {
                img.set(x, y, lin(dark_enc));
            }
        }
        let p = Params { base: base_enc, dark_auto: true, ..Params::default() };
        let measured = measure_dark_linear_rec2020(&img);
        assert!(measured.iter().all(|v| *v > 0.0), "{measured:?}");
        apply_linear_rec2020(&mut img, &p);
        let out_dark = linear_rec2020_to_srgb_encoded(img.get(9, 0));
        assert!(out_dark.iter().all(|v| *v > 0.97), "near-min should map to white, got {out_dark:?}");
        // With the placeholder dark the same frame stays well below white at the dark end.
        let mut img2 = Rgb32f::filled(w, h, lin(mid_enc));
        for y in 0..h {
            for x in 0..4 {
                img2.set(x, y, lin(base_enc));
            }
            for x in 8..12 {
                img2.set(x, y, lin(dark_enc));
            }
        }
        let p2 = Params { base: base_enc, ..Params::default() };
        apply_linear_rec2020(&mut img2, &p2);
        let placeholder_dark = linear_rec2020_to_srgb_encoded(img2.get(9, 0));
        assert!(placeholder_dark[0] < 0.9, "placeholder should not reach white, got {placeholder_dark:?}");
    }
}

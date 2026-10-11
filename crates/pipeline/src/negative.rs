//! The film-negative conversion as a scene-linear stage.
//!
//! When a photo's [`Negative`](lightcraft_develop::Negative) settings are enabled the source is a
//! negative scan: a rendered file whose own encoding is sRGB. The confirmed invert window acts on
//! the *encoded* values, so [`apply`] round-trips the sampled scene-linear Rec.2020 image to
//! encoded sRGB, runs the conversion (`lightcraft_negative::convert`), and returns it to
//! scene-linear Rec.2020 for the rest of the pipeline.
//!
//! The film base is measured once by `develop.negativeMeasure` and stored in the settings; the dark
//! endpoint is an explicit parameter (see `lightcraft_negative`). A camera profile is applied only
//! when the settings name a user-supplied `.dcp` path.

use lightcraft_develop::DevelopSettings;
use lightcraft_negative::{ColorDensity, Lut, Params, PresetParams, ProcessOrder, ToneProfile};
use lightcraft_raster::Rgb32f;

/// The conversion parameters a photo's settings describe.
pub fn params(s: &DevelopSettings) -> Params {
    let n = &s.negative;
    let preset = PresetParams {
        saturation: n.saturation as f32,
        color_density: ColorDensity::from_code(n.color_density),
        black_clip: n.black_clip as f32,
        white_clip: n.white_clip as f32,
        tone_profile: ToneProfile::from_code(n.tone_profile),
        lights: n.lights as f32,
        darks: n.darks as f32,
        whites: n.whites as f32,
        blacks: n.blacks as f32,
        lab_fade: n.lab_fade as f32,
        lab_glow: n.lab_glow as f32,
        shadows_range: n.shadows_range as f32,
        highs_range: n.highs_range as f32,
        red_cyan: [n.red_cyan_shadows as f32, n.red_cyan_mids as f32, n.red_cyan_highs as f32],
        green_magenta: [n.green_magenta_shadows as f32, n.green_magenta_mids as f32, n.green_magenta_highs as f32],
        blue_yellow: [n.blue_yellow_shadows as f32, n.blue_yellow_mids as f32, n.blue_yellow_highs as f32],
        process_order: ProcessOrder::from_code(n.process_order),
        lut: Lut::from_code(n.lut),
    };

    Params {
        base: [n.base_r as f32, n.base_g as f32, n.base_b as f32],
        dark: [n.dark_r as f32, n.dark_g as f32, n.dark_b as f32],
        dark_auto: n.dark_auto,
        preset,
        scan_gamma: n.scan_gamma as f32,
        output_gamma: n.output_gamma as f32,
        profile: lightcraft_negative::profiles::get(&n.profile),
        profile_amount: (n.profile_amount / 100.0) as f32,
        ..Params::default()
    }
}

/// Convert `img` (scene-linear Rec.2020) in place when the settings ask for a negative conversion.
///
/// The film base is normally measured once from the full frame and stored by
/// `develop.negativeMeasure`. Any other route that turns the conversion on — a settings file, a
/// preset, copy/paste, an older session — can leave the base unmeasured; rather than render a
/// degenerate window (`(0 − in)/(0 − dark)`), the base is then measured here from this frame's own
/// border, so a missing base can never become a garbage positive.
pub fn apply(img: &mut Rgb32f, s: &DevelopSettings) {
    if !s.negative.enabled || !s.section_enabled("negative") {
        return;
    }
    let mut p = params(s);
    if !s.negative.has_base() {
        p.base = lightcraft_negative::measure_base_linear_rec2020(img, s.negative.border as f32);
    }
    lightcraft_negative::apply_linear_rec2020(img, &p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_without_a_base_measures_it_instead_of_rendering_garbage() {
        // The settings path can enable the conversion with base 0 (no command, no preset
        // measurement). The stage must measure the film base from the border, not invert with a
        // degenerate `(0 - in)/(0 - dark)` window (which read the base strip as bright yellow).
        let base_enc = [0.84f32, 0.66, 0.47];
        let inner_enc = [0.35f32, 0.30, 0.25];
        let base = lightcraft_negative::srgb_encoded_to_linear_rec2020(base_enc);
        let inner = lightcraft_negative::srgb_encoded_to_linear_rec2020(inner_enc);
        let (w, h) = (20usize, 20usize);
        let mut img = Rgb32f::filled(w, h, inner);
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    img.set(x, y, base);
                }
            }
        }
        let mut s = DevelopSettings::default();
        s.negative.enabled = true; // no base: has_base() is false
        assert!(!s.negative.has_base());
        apply(&mut img, &s);
        let enc = |p: [f32; 3]| lightcraft_negative::linear_rec2020_to_srgb_encoded(p);
        let strip = enc(img.get(0, 0));
        let inside = enc(img.get(10, 10));
        for c in 0..3 {
            assert!(strip[c] < 0.02, "border should read black, got {strip:?}");
        }
        for c in 0..3 {
            assert!(inside[c] > strip[c] + 0.2, "interior should be brighter than the border, got {inside:?}");
            assert!(inside[c].is_finite() && (0.0..=1.0).contains(&inside[c]));
        }
    }

    #[test]
    fn disabled_is_untouched() {
        let s = DevelopSettings::default();
        let mut img = Rgb32f::filled(3, 1, [0.2, 0.3, 0.4]);
        let before = img.clone();
        apply(&mut img, &s);
        assert_eq!(img, before);
    }

    #[test]
    fn enabled_params_read_the_controls() {
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        s.negative.base_r = 0.84;
        s.negative.base_g = 0.66;
        s.negative.base_b = 0.47;
        s.negative.tone_profile = 1;
        s.negative.saturation = 10.0;
        let p = params(&s);
        assert_eq!(p.base, [0.84, 0.66, 0.47]);
        assert_eq!(p.preset.saturation, 10.0);
        assert!(matches!(p.preset.tone_profile, ToneProfile::Linear));
    }

    #[test]
    fn a_grey_at_the_base_inverts_to_black() {
        // The base is measured per channel; a pixel at the base reads as black.
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        s.negative.base_r = 0.84;
        s.negative.base_g = 0.66;
        s.negative.base_b = 0.47;
        // A scene-linear value that round-trips to exactly the base in encoded sRGB.
        let enc = [0.84f32, 0.66, 0.47];
        let lin = lightcraft_negative::srgb_encoded_to_linear_rec2020(enc);
        let mut img = Rgb32f::filled(2, 1, lin);
        apply(&mut img, &s);
        // round trip through encoded sRGB is within a code value, so "black" is a small residual
        for c in 0..3 {
            let back = lightcraft_negative::linear_rec2020_to_srgb_encoded(img.data[0])[c];
            assert!(back.abs() < 0.02, "chan {c}: {back}");
        }
    }
}

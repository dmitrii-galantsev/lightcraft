//! Pixel tests of the local (mask) Noise, Moiré and Defringe sliders.

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const W: usize = 240;
const H: usize = 120;

/// A deterministic hash in −1..1.
fn noise(x: usize, y: usize, k: u32) -> f32 {
    let mut v = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ k.wrapping_mul(0xcb1a_b31f);
    v ^= v >> 13;
    v = v.wrapping_mul(0x5bd1_e995);
    v ^= v >> 15;
    (v & 0xffff) as f32 / 32768.0 - 1.0
}

/// Settings with one mask covering the left half (x < 0.45), with `adjust`.
fn left_mask(adjust: LocalAdjustments) -> DevelopSettings {
    let shape = MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.46, 0.5) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], adjust, ..Default::default() });
    s
}

fn shot(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

/// Standard deviation of `f(pixel)` over columns `x0..x1` (rows 4..H-4).
fn spread(img: &Rgba8, x0: usize, x1: usize, f: impl Fn([u8; 4]) -> f32) -> f32 {
    let v: Vec<f32> = (4..H - 4).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| f(img.data[y * W + x])).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn chroma(p: [u8; 4]) -> f32 {
    p[0] as f32 - p[2] as f32
}

#[test]
fn local_noise_smooths_luminance_noise_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| [0.18 * (1.0 + 0.12 * noise(x, y, 1)); 3]);
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, luma);
    let on = shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() }));
    let off = shot(&src, &left_mask(LocalAdjustments { noise: -100.0, ..Default::default() }));
    let (smoothed, boosted) = (spread(&on, 10, 90, luma), spread(&off, 10, 90, luma));
    assert!(smoothed < 0.6 * base, "noise +100: {smoothed} vs {base}");
    assert!(boosted > 1.2 * base, "noise −100: {boosted} vs {base}");
    // outside the mask nothing changes
    assert_eq!(spread(&on, 150, 230, luma), spread(&plain, 150, 230, luma));
    // the mean brightness is kept
    let mean = |img: &Rgba8| (4..H - 4).flat_map(|y| (10..90).map(move |x| luma(img.data[y * W + x]))).sum::<f32>() / (80 * (H - 8)) as f32;
    assert!((mean(&on) - mean(&plain)).abs() < 2.0);
}

#[test]
fn local_moire_removes_colour_speckle_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let n = 0.25 * noise(x, y, 2);
        [0.18 * (1.0 + n), 0.18, 0.18 * (1.0 - n)]
    });
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, chroma);
    let on = shot(&src, &left_mask(LocalAdjustments { moire: 100.0, ..Default::default() }));
    let reduced = spread(&on, 10, 90, chroma);
    assert!(reduced < 0.4 * base, "moiré +100: {reduced} vs {base}");
    assert_eq!(spread(&on, 150, 230, chroma), spread(&plain, 150, 230, chroma));
    // luminance is kept
    assert!((spread(&on, 10, 90, luma) - spread(&plain, 10, 90, luma)).abs() < 1.0);
    // positive Noise also calms colour noise (less than Moiré)
    let n = spread(&shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() })), 10, 90, chroma);
    assert!(n < 0.8 * base && n > reduced, "{n}");
}

#[test]
fn colour_nr_removes_chroma_noise_without_raising_green() {
    // A grey with R/B chroma speckle over a green-only texture: the colour sliders alone must calm
    // the chroma and leave the green texture (and the luminance) alone.
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let n = 0.2 * noise(x, y, 5);
        let g = 0.16 + 0.03 * noise(x, y, 9);
        [g * (1.0 + n), g, g * (1.0 - n)]
    });
    let mut s = DevelopSettings::default();
    s.detail.nr_color = 25.0;
    let (plain, on) = (shot(&src, &DevelopSettings::default()), shot(&src, &s));
    let (cr0, cr1) = (spread(&plain, 10, 230, chroma), spread(&on, 10, 230, chroma));
    assert!(cr1 < 0.6 * cr0, "chroma noise {cr0} -> {cr1}");
    // Green is the anchor: the old rgb/Y basis re-applied the noise-carrying luminance and made
    // every channel (green included) noisier as the blend deepened. A real green texture survives.
    let green = |img: &Rgba8| spread(img, 10, 230, |p| p[1] as f32);
    assert!((green(&on) - green(&plain)).abs() <= 0.1 * green(&plain) + 0.5, "green {} -> {}", green(&plain), green(&on));
    // and a flat coloured region keeps its colour (only the noise goes)
    let src = Rgb32f::filled(W, H, [0.24, 0.10, 0.06]);
    let plain = shot(&src, &DevelopSettings::default());
    let on = shot(&src, &s);
    let mean = |img: &Rgba8, c: usize| {
        (4..H - 4).flat_map(|y| (10..230).map(move |x| (x, y))).map(|(x, y)| img.data[y * W + x][c] as f32).sum::<f32>() / (220 * (H - 8)) as f32
    };
    for c in 0..3 {
        assert!((mean(&on, c) - mean(&plain, c)).abs() < 2.0, "channel {c} colour shift: {} -> {}", mean(&plain, c), mean(&on, c));
    }
}

#[test]
fn luminance_nr_calms_luma_noise_and_keeps_a_bright_dark_edge() {
    // A grey field with luminance speckle and a bright|dark step: Noise Reduction calms the
    // flat-area luminance noise without softening the step (the self-guided filter's eps follows
    // the measured noise, so an edge of several EV survives).
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let base = if x < W / 2 { 0.14 } else { 0.30 };
        let n = 0.25 * noise(x, y, 11);
        [base * (1.0 + n); 3]
    });
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 14.0;
    s.detail.nr_color = 0.0;
    let plain = shot(&src, &DevelopSettings::default());
    let on = shot(&src, &s);
    let (n0, n1) = (spread(&plain, 10, 90, luma), spread(&on, 10, 90, luma));
    assert!(n1 < 0.85 * n0, "luma noise {n0} -> {n1}");
    let side = |img: &Rgba8, x0: usize, x1: usize| {
        (4..H - 4).flat_map(|y| (x0..x1).map(move |x| luma(img.data[y * W + x]))).sum::<f32>() / ((x1 - x0) * (H - 8)) as f32
    };
    let (dl, dr) = (side(&on, 20, 70), side(&on, 170, 220));
    assert!(dr - dl > 15.0, "{dl} vs {dr}");
}

#[test]
fn colour_nr_at_high_values_does_not_bleed_across_a_colour_edge() {
    // red | grey step at x = 140, both sides carrying chroma speckle (so the filter is actually
    // smoothing): raising Color must not push red into the grey side — issue #815's "fringes at
    // high values", which a plain blur shows and an edge-aware one does not.
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let n = 0.12 * noise(x, y, 3);
        let base = if x < 140 { [0.5, 0.05, 0.05] } else { [0.5; 3] };
        [base[0] * (1.0 + n), base[1], base[2] * (1.0 - n)]
    });
    let mut s = DevelopSettings::default();
    s.detail.nr_color = 100.0;
    let plain = shot(&src, &DevelopSettings::default());
    let on = shot(&src, &s);
    // red's R−G on the near side is the edge contrast; a few px into the grey side is the bleed
    let red = |img: &Rgba8| (4..H - 4).map(|y| img.data[y * W + 130][0] as f32 - img.data[y * W + 130][1] as f32).sum::<f32>() / (H - 8) as f32;
    let bleed = |img: &Rgba8| (4..H - 4).map(|y| img.data[y * W + 152][0] as f32 - img.data[y * W + 152][1] as f32).sum::<f32>() / (H - 8) as f32;
    let (b, r) = (bleed(&on), red(&plain));
    // a plain blur of radius 15 px pushes several percent of the 228-LSB contrast into the grey
    assert!(b < 0.03 * r, "colour bled {b} into a {r}-contrast edge");
}

#[test]
fn local_defringe_desaturates_purple_edges_only() {
    // dark | bright edge at x = 40 (inside the mask) and x = 200 (outside), each with a purple
    // fringe column; a flat purple patch (no edge) at x 60..80
    let src = Rgb32f::from_fn(W, H, |x, _| {
        let fringe = x == 39 || x == 40 || x == 199 || x == 200;
        if fringe {
            [0.25, 0.04, 0.3]
        } else if (60..80).contains(&x) {
            [0.2, 0.08, 0.22]
        } else if (40..120).contains(&x) || x >= 200 {
            [0.7; 3]
        } else {
            [0.02; 3]
        }
    });
    let s = left_mask(LocalAdjustments { defringe: 100.0, ..Default::default() });
    let (plain, on) = (shot(&src, &DevelopSettings::default()), shot(&src, &s));
    let sat = |p: [u8; 4]| p[0].max(p[2]) as f32 - p[1] as f32;
    let at = |img: &Rgba8, x: usize| img.data[60 * W + x];
    assert!(sat(at(&plain, 40)) > 40.0, "a purple fringe to start with: {:?}", at(&plain, 40));
    assert!(sat(at(&on, 40)) < 0.5 * sat(at(&plain, 40)), "fringe in the mask: {:?} vs {:?}", at(&on, 40), at(&plain, 40));
    assert_eq!(at(&on, 200), at(&plain, 200), "outside the mask");
    let flat = |img: &Rgba8| sat(at(img, 70));
    assert!((flat(&on) - flat(&plain)).abs() <= 2.0, "flat purple kept: {} vs {}", flat(&on), flat(&plain));
}

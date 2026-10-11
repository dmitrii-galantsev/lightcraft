//! Write the frame NLP analyses for a film negative: the raw developed for the photo's
//! `Temperature`/`Tint`, encoded to the space the invert window acts in, with the user's camera
//! profile applied and **no invert**. The output is a binary `u32 width, u32 height` header
//! followed by interleaved little-endian `f32` RGB.
//!
//! The white balance is the pipeline's own (`lin_cpu`): for an explicit `Temperature`/`Tint` on a
//! negative it is the film-base neutralization (`local::wb_matrix_for`), so the measured base is
//! stored into the settings before it, and the post-WB base is measured on the film-region pixels
//! the as-shot base was measured on (after the neutralization the base is a neutral grey that the
//! border statistic could not pick out again).
//!
//! ```text
//! cargo run --release -p lightcraft-cli --example neg_analysis -- \
//!     DSC04256.ARW -o frame.f32 --temp 2000 --tint -28 \
//!     --profile "Negative Lab - Sony ILCE-7M4 v2.3.dcp"
//! ```

use std::io::Write;

use lightcraft_develop::{DevelopSettings, WbMode};
use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::media::SourceLevel;
use lightcraft_negative::{Image, WorkingSpace, apply_profile, linear_rec2020_to_srgb_encoded, profiles};
use lightcraft_raster::Rgb32f;
use serde_json::json;

fn arg_of<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn write_f32(path: &str, img: &Rgb32f) {
    let mut bytes = Vec::with_capacity(8 + img.data.len() * 12);
    bytes.extend_from_slice(&(img.width as u32).to_le_bytes());
    bytes.extend_from_slice(&(img.height as u32).to_le_bytes());
    for p in &img.data {
        for c in 0..3 {
            bytes.extend_from_slice(&p[c].to_le_bytes());
        }
    }
    match std::fs::File::create(path).and_then(|mut f| f.write_all(&bytes)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("neg_analysis: cannot write {path}: {e}");
            std::process::exit(1);
        }
    }
}

/// The per-channel high quantile over the film-region pixels of `img`: the ring the as-shot base
/// was measured at, restricted to its warm pixels when those carried the base (the neutralized
/// base is a grey the border statistic could not re-find). `as_shot` is the encoded as-shot frame
/// the mask is derived from; `border` is the inset the base was measured at (`None` = outermost).
fn film_base(img: &Image, as_shot: &Image, border: Option<f32>) -> [f32; 3] {
    let (w, h) = (as_shot.width, as_shot.height);
    let b = border.unwrap_or(0.0).clamp(0.0, 0.49);
    let bx = ((w as f32 * b) as usize).clamp(1, w.div_ceil(2));
    let by = ((h as f32 * b) as usize).clamp(1, h.div_ceil(2));
    let warm = |p: [f32; 3]| (p[0] - p[2]) > lightcraft_negative::BASE_WARM_SPREAD && p[0] > lightcraft_negative::BASE_WARM_MIN;
    let (mut n, mut warm_n) = (0u64, 0u64);
    for (i, p) in as_shot.pixels.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        if x < bx || x >= w.saturating_sub(bx) || y < by || y >= h.saturating_sub(by) {
            n += 1;
            if warm(*p) {
                warm_n += 1;
            }
        }
    }
    let use_warm = warm_n as f32 / n.max(1) as f32 >= lightcraft_negative::BASE_MIN_WARM_FRACTION;
    let mut vals: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (i, p) in img.pixels.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        let in_ring = x < bx || x >= w.saturating_sub(bx) || y < by || y >= h.saturating_sub(by);
        if in_ring && (!use_warm || warm(as_shot.pixels[i])) {
            for c in 0..3 {
                vals[c].push(p[c].clamp(0.0, 1.0));
            }
        }
    }
    let mut out = [0f32; 3];
    for c in 0..3 {
        vals[c].sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out[c] = if vals[c].is_empty() { 0.0 } else { vals[c][(((vals[c].len() - 1) as f32) * lightcraft_negative::BASE_QUANTILE).round() as usize] };
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(input) = args.first().cloned() else {
        eprintln!("usage: neg_analysis <raw> -o <out.f32> [--temp K] [--tint N] [--profile PATH]");
        std::process::exit(2);
    };
    let Some(out) = arg_of(&args, "-o").map(str::to_string) else {
        eprintln!("neg_analysis: missing -o");
        std::process::exit(2);
    };
    let temp: f64 = arg_of(&args, "--temp").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let tint: f64 = arg_of(&args, "--tint").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let profile = arg_of(&args, "--profile").unwrap_or("").to_string();

    let mut s = Session::new().with_fs().with_system_clock();
    let r = match s.execute("library.import", &json!({"paths": [input]})) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("neg_analysis: import failed: {e}");
            std::process::exit(1);
        }
    };
    let Some(id) = r["imported"].as_array().and_then(|a| a.first()).and_then(serde_json::Value::as_u64) else {
        eprintln!("neg_analysis: not a readable photo: {input}");
        std::process::exit(1);
    };
    let id = PhotoId(id);

    let mut settings = DevelopSettings::default();
    if temp > 0.0 {
        settings.wb.mode = WbMode::Custom;
        settings.wb.temp = temp;
        settings.wb.tint = tint;
    }

    let src = match s.source_now(id, SourceLevel::Preview) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("neg_analysis: decode failed: {e}");
            std::process::exit(1);
        }
    };
    let info = s.source_info(id);
    let (as_shot_base, border) = lightcraft_negative::measure_base_linear_rec2020_full(&src, -1.0);

    // An explicit white balance on a negative is the film-base neutralization: the pipeline
    // measures the base first and the render applies the gains, so the harness does the same.
    if temp > 0.0 {
        settings.negative.enabled = true;
        if as_shot_base.iter().all(|v| *v > 0.0) {
            settings.negative.base_r = as_shot_base[0] as f64;
            settings.negative.base_g = as_shot_base[1] as f64;
            settings.negative.base_b = as_shot_base[2] as f64;
        }
    }

    let as_shot_enc: Rgb32f = {
        let mut e: Rgb32f = (*src).clone();
        for px in &mut e.data {
            *px = linear_rec2020_to_srgb_encoded(*px);
        }
        e
    };

    let mut enc: Rgb32f = (*src).clone();
    lightcraft_pipeline::local::white_balance(&mut enc, &info, &settings);
    for px in &mut enc.data {
        *px = linear_rec2020_to_srgb_encoded(*px);
    }
    let enc_img = Image { width: enc.width, height: enc.height, pixels: enc.data.clone() };
    let as_shot_img = Image { width: as_shot_enc.width, height: as_shot_enc.height, pixels: as_shot_enc.data.clone() };
    let wb_base = film_base(&enc_img, &as_shot_img, border);

    let frame = match profiles::get(&profile) {
        Some(dcp) => {
            let profiled = apply_profile(&enc_img, &dcp, lightcraft_negative::color::D50_XY, WorkingSpace::ProPhoto, 1.0);
            Rgb32f { width: profiled.width, height: profiled.height, data: profiled.pixels }
        }
        None => enc.clone(),
    };
    let profile_base = film_base(&Image { width: frame.width, height: frame.height, pixels: frame.data.clone() }, &as_shot_img, border);

    write_f32(&out, &frame);
    println!(
        "{}",
        json!({
            "file": std::path::Path::new(&input).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            "out": out,
            "w": frame.width,
            "h": frame.height,
            "as_shot_base": as_shot_base,
            "wb_base": wb_base,
            "profile_base": profile_base,
            "profile": !profile.is_empty(),
        })
    );
}

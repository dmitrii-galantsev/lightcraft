//! Reproduces the negative-conversion evidence table in `docs/negative-conversion.md` against the
//! reverse-engineering fixtures (a synthetic negative with a known positive, and the plugin's own
//! export of the same input).
//!
//! The fixtures live outside this repository. Point `NLP_RE_DIR` at the `nlp-re` tree (default
//! `$HOME/git/ai/nlp-re`); without it this test skips, like the tests that need craft-fonts. Run
//! with `--nocapture` to print the table:
//!
//! ```text
//! NLP_RE_DIR=~/git/ai/nlp-re cargo +stable test -p lightcraft-cli \
//!     --test negative_reference -- --nocapture
//! ```
//!
//! Every row uses the same settings and the same command as the doc; the only per-row difference is
//! the measured film base. The scene region is the right 94 % (the left 6 % is the film-base strip).

use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

const BIN: &str = env!("CARGO_BIN_EXE_lightcraft-cli");

/// The `nlp-re` tree, or `None` when it is not present.
fn nlp_re_dir() -> Option<PathBuf> {
    let dir =
        std::env::var_os("NLP_RE_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("git/ai/nlp-re")))?;
    dir.join("testdata/negatives/grey_wedge_neg8.png").is_file().then_some(dir)
}

struct Fixture {
    name: &'static str,
    base8: [f64; 3],
}

const FIXTURES: &[Fixture] = &[
    Fixture { name: "grey_wedge", base8: [214.0, 169.0, 120.0] },
    Fixture { name: "color_grid", base8: [200.0, 155.0, 110.0] },
    Fixture { name: "channel_ramp", base8: [229.0, 198.0, 151.0] },
    Fixture { name: "photo_landscape", base8: [214.0, 169.0, 120.0] },
];

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-neg-ref-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn srgb(path: &std::path::Path) -> lightcraft_raster::Rgba8 {
    lightcraft_codecs::decode(&std::fs::read(path).unwrap(), Default::default()).unwrap().to_srgb8()
}

/// The film base `develop.negativeMeasure` records for `neg` (encoded 0..1).
fn measure_base(neg: &std::path::Path) -> [f64; 3] {
    let o = Command::new(BIN).args(["run", "--import", neg.to_str().unwrap(), "develop.negativeMeasure"]).output().unwrap();
    assert!(o.status.success(), "develop.negativeMeasure failed: {}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout);
    let line = stdout.lines().last().unwrap_or("{}");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let a = v["result"]["base"].as_array().unwrap();
    [a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_f64().unwrap()]
}

/// Mean absolute resid / RMS / max of `a` vs `b` per channel over the right 94 %.
fn stats(a: &lightcraft_raster::Rgba8, b: &lightcraft_raster::Rgba8) -> ([f64; 3], [f64; 3], [f64; 3]) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let w = a.width;
    let x0 = (w as f64 * 0.06) as usize;
    let mut kept = 0.0;
    let (mut bias, mut sq, mut mx) = ([0.0f64; 3], [0.0f64; 3], [0.0f64; 3]);
    for (idx, (p, q)) in a.data.iter().zip(&b.data).enumerate() {
        if idx % w < x0 {
            continue;
        }
        for c in 0..3 {
            let d = p[c] as f64 - q[c] as f64;
            bias[c] += d;
            sq[c] += d * d;
            mx[c] = mx[c].max(d.abs());
        }
        kept += 1.0;
    }
    (bias.map(|v| v / kept), sq.map(|v| (v / kept).sqrt()), mx)
}

#[test]
fn reproduces_the_documented_table() {
    let Some(dir) = nlp_re_dir() else {
        eprintln!("negative_reference: skipped (set NLP_RE_DIR to the nlp-re tree)");
        return;
    };
    for f in FIXTURES {
        let neg = dir.join(format!("testdata/negatives/{}_neg8.png", f.name));
        if !neg.is_file() {
            continue;
        }
        let [r, g, b] = f.base8;
        // The film base is measured by the pipeline itself (the settings carry no base): the
        // documented table is what a user gets with `{"negative": {"enabled": true}}` and the
        // preset settings below. First assert the statistic matches the fixture's uniform strip.
        let measured = measure_base(&neg);
        for (c, want) in measured.iter().zip([r, g, b]) {
            assert!((c * 255.0 - want).abs() <= 1.0, "{}: measured base {:?} != {want}", f.name, measured);
        }
        // The exact settings of the documented table (no `base_*`: measured), a linear tone profile,
        // no saturation, neutral density, the Natural LUT, colour-first, gamma 1 in/out.
        let settings = tmp(&format!("{}.json", f.name));
        std::fs::write(
            &settings,
            serde_json::to_vec(&json!({"negative": {
                "enabled": true,
                "tone_profile": 1, "saturation": 0.0, "color_density": 1, "lut": 2,
                "process_order": 0, "scan_gamma": 1.0, "output_gamma": 1.0
            }}))
            .unwrap(),
        )
        .unwrap();
        let out = tmp(&format!("lc_{}.png", f.name));
        let st = Command::new(BIN)
            .args(["render", neg.to_str().unwrap(), "-o", out.to_str().unwrap(), "--settings", settings.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(st.success());
        let mine = srgb(&out);
        let plugin = dir.join(format!("probe/run-01/{}_neg16.tif", f.name));
        let positive = dir.join(format!("testdata/positives/{}.png", f.name));
        let (_, pos_rms, _) = stats(&mine, &srgb(&positive));
        let (nlp_rms, nlp_max) = if plugin.is_file() {
            let (_, rms, mx) = stats(&mine, &srgb(&plugin));
            println!(
                "{:14} vs plugin  RMS {:.2}/{:.2}/{:.2}  max {:.0}/{:.0}/{:.0}   vs positive RMS {:.2}/{:.2}/{:.2}",
                f.name, rms[0], rms[1], rms[2], mx[0], mx[1], mx[2], pos_rms[0], pos_rms[1], pos_rms[2]
            );
            (Some(rms), Some(mx))
        } else {
            println!(
                "{:14} vs plugin  (no export)                             vs positive RMS {:.2}/{:.2}/{:.2}",
                f.name, pos_rms[0], pos_rms[1], pos_rms[2]
            );
            (None, None)
        };
        if f.name == "grey_wedge" {
            let rms = nlp_rms.expect("grey_wedge has a plugin export");
            let mx = nlp_max.expect("grey_wedge has a plugin export");
            assert!(rms[0] <= 2.5 && rms[1] <= 2.5 && rms[2] <= 6.0, "grey_wedge vs plugin RMS regressed: {rms:?}");
            assert!(mx.iter().all(|v| *v <= 12.0), "grey_wedge vs plugin max regressed: {mx:?}");
        }
    }
}

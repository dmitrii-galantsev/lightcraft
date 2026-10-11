//! Negative-conversion commands.
//!
//! The conversion's parameters are ordinary develop controls (`negative.*`, see
//! `lightcraft_develop::controls`), so `develop.set` / `develop.adjust` address them and the CLI,
//! control channel and MCP reach them like any other slider. These commands cover the parts a
//! control cannot: the master switch, measuring the film base from the frame, and choosing the
//! user-supplied camera profile.

use serde_json::{Value, json};

use super::{CommandSpec, bad, bool_or, cmd, f64_or, has_active, str_param};
use crate::{Result, Session, media::SourceLevel};

/// Modify the active photo's settings with `f` and commit.
fn edit(s: &mut Session, c: &str, label: &str, f: impl FnOnce(&mut lightcraft_develop::DevelopSettings) -> Result<()>) -> Result<()> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    f(&mut d)?;
    s.set_develop(id, d, label)
}

/// Measure the active photo's film base from its preview source.
fn measure(s: &mut Session, c: &str, border: f64) -> Result<[f32; 3]> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(c, e))?;
    Ok(lightcraft_negative::measure_base_linear_rec2020(&src, border.clamp(0.0, 0.49) as f32))
}

/// `(r, g, b)` of an array of three numbers, or `None`.
fn triple(v: Option<&Value>) -> Option<[f64; 3]> {
    let a = v?.as_array()?;
    let v: Vec<f64> = a.iter().map(Value::as_f64).collect::<Option<_>>()?;
    match v.as_slice() {
        [r, g, b] => Some([*r, *g, *b]),
        _ => None,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.negative",
            "Convert Negative",
            ["Photo"],
            None,
            "{enabled?: bool (default: toggle), measure?: bool (default: true when enabling without a measured base)} — turns a film-negative scan into a positive; when enabled without a base it is measured from the frame border → {enabled, base}",
            has_active,
            |s, p| {
                let c = "develop.negative";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let current = s.develop_of(id).unwrap_or_default();
                let enabled = bool_or(p, "enabled", !current.negative.enabled);
                let want_measure = bool_or(p, "measure", enabled && !current.negative.has_base());
                let measured = if enabled && want_measure { Some(measure(s, c, current.negative.border)?) } else { None };
                edit(s, c, if enabled { "Convert Negative" } else { "Negative Conversion Off" }, |d| {
                    if let Some(b) = measured {
                        d.negative.base_r = b[0] as f64;
                        d.negative.base_g = b[1] as f64;
                        d.negative.base_b = b[2] as f64;
                    }
                    d.negative.enabled = enabled;
                    Ok(())
                })?;
                let d = s.develop_of(id).unwrap_or_default();
                Ok(json!({
                    "enabled": d.negative.enabled,
                    "base": [d.negative.base_r, d.negative.base_g, d.negative.base_b],
                    "measured": measured.is_some(),
                }))
            }
        ),
        cmd!(
            "develop.negativeMeasure",
            "Measure Film Base",
            ["Photo"],
            None,
            "{border?: number 0..49 (default: the current negative.border)} — measures the film base from the frame's border ring (its per-channel 95th percentile) and stores it per channel → {base: [r,g,b] (encoded 0..1)}",
            has_active,
            |s, p| {
                let c = "develop.negativeMeasure";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let current = s.develop_of(id).unwrap_or_default();
                let border = f64_or(p, "border", current.negative.border);
                let base = measure(s, c, border)?;
                edit(s, c, "Measure Film Base", |d| {
                    d.negative.base_r = base[0] as f64;
                    d.negative.base_g = base[1] as f64;
                    d.negative.base_b = base[2] as f64;
                    Ok(())
                })?;
                Ok(json!({"base": [base[0], base[1], base[2]]}))
            }
        ),
        cmd!(
            "develop.negativeDark",
            "Negative Dark Endpoint",
            [],
            None,
            "{dark: [r,g,b] encoded 0..1} — the invert window's dark endpoint, an explicit unfitted parameter (see docs/negative-conversion.md) → {dark}",
            has_active,
            |s, p| {
                let c = "develop.negativeDark";
                let dark = triple(p.get("dark")).ok_or_else(|| bad(c, "give `dark` as [r,g,b]"))?;
                if !dark.iter().all(|v| v.is_finite()) {
                    return Err(bad(c, "`dark` must be three finite numbers"));
                }
                edit(s, c, "Negative Dark Endpoint", |d| {
                    d.negative.dark_r = dark[0].clamp(-0.1, 1.0);
                    d.negative.dark_g = dark[1].clamp(-0.1, 1.0);
                    d.negative.dark_b = dark[2].clamp(-0.1, 1.0);
                    Ok(())
                })?;
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let d = s.develop_of(id).unwrap_or_default();
                Ok(json!({"dark": [d.negative.dark_r, d.negative.dark_g, d.negative.dark_b]}))
            }
        ),
        cmd!(
            "develop.negativeProfile",
            "Negative Camera Profile",
            [],
            None,
            "{path: string — a user-supplied .dcp file; empty clears it} — the camera profile applied after the invert. LightCraft ships no profile → {path, description?}",
            has_active,
            |s, p| {
                let c = "develop.negativeProfile";
                let path = str_param(p, "path").unwrap_or("").trim().to_string();
                let description = if path.is_empty() {
                    None
                } else {
                    let dcp = lightcraft_negative::profiles::load(std::path::Path::new(&path)).map_err(|e| bad(c, e))?;
                    Some(lightcraft_negative::profiles::describe(&dcp))
                };
                edit(s, c, "Negative Camera Profile", |d| {
                    d.negative.profile = path.clone();
                    Ok(())
                })?;
                Ok(json!({"path": path, "description": description}))
            }
        ),
        cmd!(
            "develop.negativeReset",
            "Reset Negative Conversion",
            [],
            None,
            "{} — back to the default negative-conversion settings (off)",
            has_active,
            |s, _| {
                edit(s, "develop.negativeReset", "Reset Negative Conversion", |d| {
                    d.negative = lightcraft_develop::Negative::default();
                    Ok(())
                })?;
                Ok(json!({"reset": true}))
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn spec_ids_are_registered() {
        for id in ["develop.negative", "develop.negativeMeasure", "develop.negativeDark", "develop.negativeProfile", "develop.negativeReset"] {
            assert!(super::specs().iter().any(|c| c.id == id), "missing {id}");
            assert!(crate::cmd::find_command(id).is_some(), "{id} not in the registry");
        }
    }
}

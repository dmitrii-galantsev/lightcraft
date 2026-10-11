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

/// The active photo's film base from its preview source. `< 0` asks the frame for the
/// film-base ring; the returned `Option<f32>` is the inset used (or the explicit one).
fn measure(s: &mut Session, c: &str, border: f64) -> Result<([f32; 3], Option<f32>)> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(c, e))?;
    Ok(lightcraft_negative::measure_base_linear_rec2020_full(&src, border as f32))
}

/// The dark endpoint the analysed frame would measure from the active photo's preview source: the
/// per-channel robust near-min. This previews the pipeline's own (post-white-balance) measurement.
fn measure_dark(s: &mut Session, c: &str) -> Result<[f32; 3]> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(c, e))?;
    Ok(lightcraft_negative::measure_dark_linear_rec2020(&src))
}

/// The active photo's camera names to try when picking a profile, best first (`Make Model`, then
/// `Model`, then `Make`), all non-empty.
fn camera_models(s: &Session, id: lightcraft_catalog::PhotoId) -> Vec<String> {
    let Some(p) = s.catalog.photo(id) else { return Vec::new() };
    let m = &p.meta;
    let mut v = vec![m.camera.clone(), m.model.clone(), m.make.clone()];
    v.retain(|s| !s.trim().is_empty());
    v.dedup();
    v
}

/// The `.dcp` tree to discover profiles under: an explicit `dir`, else the standard Camera Raw
/// roots this machine exposes.
fn profile_files(dir: Option<&str>) -> Vec<lightcraft_negative::ProfileFile> {
    match dir.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => lightcraft_negative::discover(std::path::Path::new(d)),
        None => lightcraft_negative::discover_default(),
    }
}

fn profile_json(f: &lightcraft_negative::ProfileFile) -> Value {
    json!({
        "name": f.name,
        "path": f.path.to_string_lossy(),
        "camera": f.camera,
        "generation": f.generation.map(|g| g.label()),
    })
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
            "{enabled?: bool (default: toggle), measure?: bool (default: true when enabling without a measured base), dark?: [r,g,b]} — turns a film-negative scan into a positive; when enabled without a base it is measured from the frame border. The dark endpoint stays at its setting (the placeholder by default; see develop.negativeDark) → {enabled, base, dark_auto}",
            has_active,
            |s, p| {
                let c = "develop.negative";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let current = s.develop_of(id).unwrap_or_default();
                let enabled = bool_or(p, "enabled", !current.negative.enabled);
                let want_measure = bool_or(p, "measure", enabled && !current.negative.has_base());
                let dark = triple(p.get("dark"));
                let measured = if enabled && want_measure { Some(measure(s, c, current.negative.border)?) } else { None };
                edit(s, c, if enabled { "Convert Negative" } else { "Negative Conversion Off" }, |d| {
                    if let Some((b, used)) = measured {
                        d.negative.base_r = b[0] as f64;
                        d.negative.base_g = b[1] as f64;
                        d.negative.base_b = b[2] as f64;
                        if let Some(border) = used {
                            d.negative.border = border as f64;
                        }
                    }
                    if let Some(k) = dark {
                        d.negative.dark_r = k[0].clamp(-0.1, 1.0);
                        d.negative.dark_g = k[1].clamp(-0.1, 1.0);
                        d.negative.dark_b = k[2].clamp(-0.1, 1.0);
                        d.negative.dark_auto = false;
                    }
                    d.negative.enabled = enabled;
                    Ok(())
                })?;
                let d = s.develop_of(id).unwrap_or_default();
                Ok(json!({
                    "enabled": d.negative.enabled,
                    "base": [d.negative.base_r, d.negative.base_g, d.negative.base_b],
                    "dark_auto": d.negative.dark_auto,
                    "measured": measured.is_some(),
                }))
            }
        ),
        cmd!(
            "develop.negativeMeasure",
            "Measure Film Base",
            ["Photo"],
            None,
            "{border?: number; <0 (default) lets the frame pick the film-base ring} — measures the film base from the frame's border ring (its per-channel 95th percentile over the ring's chromatic pixels, so a holder's neutral margin is not read as the base) and stores it per channel → {base: [r,g,b] (encoded 0..1), border: the inset used or null when the ring could not be derived}",
            has_active,
            |s, p| {
                let c = "develop.negativeMeasure";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let current = s.develop_of(id).unwrap_or_default();
                let border = f64_or(p, "border", current.negative.border);
                let (base, used) = measure(s, c, border)?;
                edit(s, c, "Measure Film Base", |d| {
                    d.negative.base_r = base[0] as f64;
                    d.negative.base_g = base[1] as f64;
                    d.negative.base_b = base[2] as f64;
                    if let Some(b) = used {
                        d.negative.border = b as f64;
                    }
                    Ok(())
                })?;
                Ok(json!({"base": [base[0], base[1], base[2]], "border": used}))
            }
        ),
        cmd!(
            "develop.negativeDark",
            "Negative Dark Endpoint",
            [],
            None,
            "{dark?: [r,g,b] encoded 0..1; auto?: bool} — set the invert window's dark endpoint. Give an explicit `dark` triple, or omit it (or pass auto:true) to measure the frame's per-channel robust near-min (see docs/negative-conversion.md) → {dark, dark_auto}",
            has_active,
            |s, p| {
                let c = "develop.negativeDark";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let dark = triple(p.get("dark"));
                let auto = bool_or(p, "auto", dark.is_none());
                if let Some(k) = dark
                    && !k.iter().all(|v| v.is_finite())
                {
                    return Err(bad(c, "`dark` must be three finite numbers"));
                }
                let estimate = if auto { Some(measure_dark(s, c)?) } else { None };
                edit(s, c, "Negative Dark Endpoint", |d| {
                    if let Some(k) = dark {
                        d.negative.dark_r = k[0].clamp(-0.1, 1.0);
                        d.negative.dark_g = k[1].clamp(-0.1, 1.0);
                        d.negative.dark_b = k[2].clamp(-0.1, 1.0);
                        d.negative.dark_auto = false;
                    } else {
                        d.negative.dark_auto = auto;
                    }
                    Ok(())
                })?;
                let d = s.develop_of(id).unwrap_or_default();
                Ok(json!({
                    "dark": [d.negative.dark_r, d.negative.dark_g, d.negative.dark_b],
                    "dark_auto": d.negative.dark_auto,
                    "dark_estimate": estimate,
                }))
            }
        ),
        cmd!(
            "develop.negativeProfiles",
            "Negative Camera Profiles",
            [],
            None,
            "{dir?: string — a .dcp tree; default: the standard Camera Raw camera-profile roots} — list the user's own profiles (nothing is bundled) → {dirs: [..], profiles: [{name, path, camera, generation?}]}",
            |_s| Ok(()),
            |_s, p| {
                let dir = str_param(p, "dir");
                let files = profile_files(dir);
                let profiles: Vec<Value> = files.iter().map(profile_json).collect();
                let dirs: Vec<String> = lightcraft_negative::default_dirs().iter().map(|d| d.to_string_lossy().into_owned()).collect();
                Ok(json!({"dirs": dirs, "profiles": profiles}))
            }
        ),
        cmd!(
            "develop.negativeProfileAuto",
            "Negative Camera Profile (Auto)",
            [],
            None,
            "{dir?: string} — pick the best profile for the active photo's camera from the user's own tree (exact camera match, newest generation) and set it. Errors when nothing matches → {path, description, camera}",
            has_active,
            |s, p| {
                let c = "develop.negativeProfileAuto";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let files = profile_files(str_param(p, "dir"));
                let models = camera_models(s, id);
                let Some(pick) = lightcraft_negative::find_for_models(&files, &models) else {
                    return Err(bad(
                        c,
                        format!(
                            "no profile for camera {:?} under {:?}",
                            models.first().cloned().unwrap_or_default(),
                            str_param(p, "dir").unwrap_or("the standard Camera Raw roots")
                        ),
                    ));
                };
                let path = pick.path.to_string_lossy().into_owned();
                let dcp = lightcraft_negative::profiles::load(&pick.path).map_err(|e| bad(c, e))?;
                let description = lightcraft_negative::profiles::describe(&dcp);
                edit(s, c, "Negative Camera Profile", |d| {
                    d.negative.profile = path.clone();
                    Ok(())
                })?;
                Ok(json!({"path": path, "description": description, "camera": pick.camera, "generation": pick.generation.map(|g| g.label())}))
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
        for id in [
            "develop.negative",
            "develop.negativeMeasure",
            "develop.negativeDark",
            "develop.negativeProfile",
            "develop.negativeProfiles",
            "develop.negativeProfileAuto",
            "develop.negativeReset",
        ] {
            assert!(super::specs().iter().any(|c| c.id == id), "missing {id}");
            assert!(crate::cmd::find_command(id).is_some(), "{id} not in the registry");
        }
    }
}

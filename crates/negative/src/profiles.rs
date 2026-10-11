//! A process-wide cache of user-supplied camera profiles.
//!
//! LightCraft ships **no** camera profile: a negative conversion's `.dcp` is a
//! file the user supplies (the same rule the project applies to AI model
//! weights — nothing bundled, nothing redistributed). A path is read once per
//! process; a missing, oversized, unreadable or malformed file simply yields
//! `None` (the profile half of the conversion is then skipped), never an error
//! the render has to handle.

use crate::dcp::Dcp;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Largest `.dcp` read (the shipped profiles are all well under a megabyte).
const MAX_FILE: u64 = 16 << 20;

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<Dcp>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Dcp>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The parsed profile for `path`, or `None` when it cannot be read or parsed.
/// Read and parsed once per process.
pub fn get(path: &str) -> Option<Arc<Dcp>> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(path) {
        return hit.clone();
    }
    let profile = load(Path::new(path)).map(Arc::new).inspect_err(|e| eprintln!("lightcraft: ignoring camera profile {e}")).ok();
    cache.insert(path.to_owned(), profile.clone());
    profile
}

/// Read and parse a `.dcp`, with a size cap and validation.
pub fn load(path: &Path) -> Result<Dcp, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_FILE {
        return Err(format!("{}: {size} bytes is too large for a camera profile", path.display()));
    }
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&data, &path.display().to_string())
}

/// Parse and validate a profile from bytes (`origin` names it errors).
///
/// Everything [`crate::convert::apply_profile`] can only consume conditionally is
/// checked here, so a profile that is accepted is one the render will actually
/// use: the colour half must be a usable 3x3 [`CameraColorSpec`], and the tone
/// curve and the hue/sat tables must build. A profile the colour half cannot
/// drive is rejected, never silently ignored at render time.
pub fn parse(data: &[u8], origin: &str) -> Result<Dcp, String> {
    let dcp = Dcp::parse(data).map_err(|e| format!("{origin}: {e}"))?;
    // A profile with no colour matrix cannot drive the colour half; reject it so
    // the caller can report why instead of silently skipping.
    if dcp.color_matrix1.is_none() {
        return Err(format!("{origin}: no ColorMatrix1 tag"));
    }
    // A 3x4 ColorMatrix1 is a four-channel (RGBE) profile: it needs a
    // ReductionMatrix that ships nowhere, so the colour half would be dropped
    // without a word. Reject it here instead.
    if crate::color::CameraColorSpec::from_dcp(&dcp).is_none() {
        return Err(format!("{origin}: ColorMatrix1 is not a usable 3x3 camera matrix (four-channel/RGBE profiles are unsupported)"));
    }
    // A tone curve that does not build is dropped silently in `apply_profile`.
    if let Some(flat) = &dcp.profile_tone_curve {
        crate::tonecurve::ToneCurve::from_pairs(flat).map_err(|e| format!("{origin}: tone curve: {e}"))?;
    }
    Ok(dcp)
}

/// A short human description of a profile: `name (generation)`, or the camera
/// model, or `unnamed`.
pub fn describe(dcp: &Dcp) -> String {
    let name = dcp.profile_name.as_deref().or(dcp.unique_camera_model.as_deref()).unwrap_or("unnamed");
    match dcp.generation() {
        Some(g) => format!("{name} ({})", g.label()),
        None => name.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Discovery — the user's own Negative Lab Pro profile tree
// ---------------------------------------------------------------------------

/// A `.dcp` found on disk, classified from its filename (no parsing, so a whole
/// tree of thousands can be listed cheaply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFile {
    pub path: std::path::PathBuf,
    /// The file's name (no directory).
    pub name: String,
    /// The camera part of the name, before a trailing generation suffix.
    pub camera: String,
    pub generation: Option<crate::dcp::Generation>,
}

impl ProfileFile {
    /// Classify a file path; `None` when it is not a `.dcp`.
    pub fn from_path(path: &Path) -> Option<ProfileFile> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().ends_with(".dcp") {
            return None;
        }
        let generation = crate::dcp::Generation::from_filename(&name);
        let stem = name.strip_suffix(".dcp").unwrap_or(&name).trim_end();
        // `Negative Lab - Sony ILCE-7M4 v2.3` -> camera `Negative Lab - Sony ILCE-7M4`.
        let camera = stem.strip_suffix(" v2.3").or_else(|| stem.strip_suffix(" v2")).unwrap_or(stem).to_owned();
        Some(ProfileFile { path: path.to_path_buf(), name, camera, generation })
    }
}

/// The per-OS roots the Adobe Camera Raw camera profiles live under, Negative Lab Pro's own tree
/// being a subdirectory of each. Existing directories only, in a stable order.
pub fn default_dirs() -> Vec<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(std::path::PathBuf::from(appdata).join("Adobe/CameraRaw/CameraProfiles"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(&home).join("Library/Application Support/Adobe/CameraRaw/CameraProfiles"));
        roots.push(std::path::PathBuf::from(&home).join(".config/Adobe/CameraRaw/CameraProfiles"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        roots.push(std::path::PathBuf::from(xdg).join("Adobe/CameraRaw/CameraProfiles"));
    }
    roots.retain(|r| r.is_dir());
    roots
}

/// Every `.dcp` under `dir` (recursively, to a bounded depth), sorted by name. Cheap: filenames
/// only, nothing parsed. A missing or unreadable directory yields an empty list.
pub fn discover(dir: &Path) -> Vec<ProfileFile> {
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<ProfileFile>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            walk(&path, depth + 1, out);
        } else if let Some(p) = ProfileFile::from_path(&path) {
            out.push(p);
        }
    }
}

/// Every profile the current machine exposes under the standard Camera Raw roots.
pub fn discover_default() -> Vec<ProfileFile> {
    let mut out: Vec<ProfileFile> = Vec::new();
    for root in default_dirs() {
        out.extend(discover(&root));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// `s` reduced to lowercase alphanumerics, for camera-name comparison ("Sony ILCE-7M4" and
/// "ILCE-7M4" both key on `ilce7m4`).
fn camera_key(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Pick the best profile for a camera among `files`: an exact camera-name match preferred, then a
/// suffix match on the model; within a match, the newest generation (v2.3, then v2, then base),
/// then the shortest name. `models` are the camera make/model strings to try, best first. `None`
/// when nothing matches or `models` names no camera.
pub fn find_for_models(files: &[ProfileFile], models: &[String]) -> Option<ProfileFile> {
    let keys: Vec<String> = models.iter().map(|m| camera_key(m)).filter(|m| !m.is_empty()).collect();
    if keys.is_empty() {
        return None;
    }
    // Lower is better: exact camera match, then best model candidate, then newest generation, then
    // shortest name.
    let mut best: Option<((usize, usize, i32, usize), ProfileFile)> = None;
    for f in files {
        let key = camera_key(&f.camera);
        let rank =
            keys.iter().position(|m| key == *m).map(|i| (0usize, i)).or_else(|| keys.iter().position(|m| key.ends_with(m)).map(|i| (1usize, i)));
        let Some((class, idx)) = rank else { continue };
        let generation = match f.generation {
            Some(crate::dcp::Generation::V23) => 2,
            Some(crate::dcp::Generation::V2) => 1,
            _ => 0,
        };
        let score = (class, idx, -generation, f.name.len());
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, f.clone()));
        }
    }
    best.map(|(_, f)| f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a classic little-endian DCP/TIFF from `(tag, type, count, value bytes)`.
    fn classic_tiff(entries: &[(u16, u16, u32, Vec<u8>)]) -> Vec<u8> {
        let ifd_off = 8u32;
        let data_start = ifd_off as usize + 2 + 12 * entries.len() + 4;
        let mut ifd = Vec::new();
        ifd.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut extra = Vec::new();
        for (tag, typ, count, val) in entries {
            ifd.extend_from_slice(&tag.to_le_bytes());
            ifd.extend_from_slice(&typ.to_le_bytes());
            ifd.extend_from_slice(&count.to_le_bytes());
            if val.len() <= 4 {
                let mut v = val.clone();
                v.resize(4, 0);
                ifd.extend_from_slice(&v);
            } else {
                ifd.extend_from_slice(&((data_start + extra.len()) as u32).to_le_bytes());
                extra.extend_from_slice(val);
            }
        }
        ifd.extend_from_slice(&0u32.to_le_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(b"IIRC");
        out.extend_from_slice(&ifd_off.to_le_bytes());
        out.extend_from_slice(&ifd);
        out.extend_from_slice(&extra);
        out
    }

    fn float_bytes(v: &[f64]) -> Vec<u8> {
        v.iter().flat_map(|x| (*x as f32).to_le_bytes()).collect()
    }

    /// A profile with a `ColorMatrix1` of `cols` columns (9 = 3x3, 12 = 3x4/RGBE).
    fn matrix_profile(cols: usize) -> Vec<u8> {
        let m: Vec<f64> = (0..3 * cols).map(|i| if i % (cols + 1) == 0 { 1.0 } else { 0.0 }).collect();
        classic_tiff(&[(crate::dcp::TAG_COLOR_MATRIX1, 11, m.len() as u32, float_bytes(&m))])
    }

    #[test]
    fn a_three_by_three_camera_matrix_loads() {
        assert!(parse(&matrix_profile(3), "3x3").is_ok());
    }

    #[test]
    fn an_rgbe_four_channel_matrix_is_rejected_not_skipped() {
        // A 3x4 ColorMatrix1 (RGBE sensor) cannot drive the colour half; `parse` must say so
        // rather than accept the profile and let the render drop it silently.
        let err = parse(&matrix_profile(4), "rgbe").unwrap_err();
        assert!(err.contains("3x3"), "{err}");
    }

    #[test]
    fn a_profile_with_a_broken_tone_curve_is_rejected() {
        // Non-increasing tone-curve inputs would be dropped silently in `apply_profile`.
        let m = float_bytes(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let curve = float_bytes(&[0.0, 0.0, 0.0, 1.0]); // two points at the same input
        let bytes = classic_tiff(&[(crate::dcp::TAG_COLOR_MATRIX1, 11, 9, m), (crate::dcp::TAG_PROFILE_TONE_CURVE, 11, 4, curve)]);
        let err = parse(&bytes, "curve").unwrap_err();
        assert!(err.contains("tone curve"), "{err}");
    }

    /// The user's own Negative Lab Pro profiles, read-only, if the tree is present.
    fn nlp_profiles_dir() -> Option<std::path::PathBuf> {
        let dir = std::env::var_os("NLP_PROFILES_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("git/ai/nlp-re/vendor/camera-profiles")))?;
        dir.is_dir().then_some(dir)
    }

    #[test]
    fn real_negative_lab_profiles_load_or_are_rejected_loudly() {
        let Some(dir) = nlp_profiles_dir() else {
            eprintln!("real_negative_lab_profiles: skipped (set NLP_PROFILES_DIR)");
            return;
        };
        let mut ok = 0usize;
        let mut rejected: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            if path.extension().is_none_or(|x| x != "dcp") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            match parse(&std::fs::read(&path).unwrap(), &name) {
                Ok(d) => {
                    // Every profile the reader accepts must carry a generation and a matrix.
                    assert!(d.generation().is_some(), "{name}: no recognised ProfileName");
                    assert!(d.color_matrix1.is_some(), "{name}: no ColorMatrix1");
                    ok += 1;
                }
                Err(e) => rejected.push(format!("{name}: {e}")),
            }
        }
        // The RGBE profiles are the only ones rejected, and each names the 3x3 reason.
        assert!(ok > 2500, "only {ok} profiles loaded");
        assert!(!rejected.is_empty(), "the RGBE profiles should be rejected, not silently skipped");
        for r in &rejected {
            assert!(r.contains("3x3"), "unexpected rejection: {r}");
        }
        // A known-good colour profile is accepted and read as v2.3.
        let sony = dir.join("Negative Lab - Sony ILCE-7M4 v2.3.dcp");
        if sony.is_file() {
            let d = parse(&std::fs::read(&sony).unwrap(), "sony").unwrap();
            assert_eq!(d.generation(), Some(crate::dcp::Generation::V23));
            assert_eq!(d.look_table.as_ref().map(|t| t.dims), Some([90, 30, 30]));
            assert!(d.profile_tone_curve.as_ref().is_some_and(|c| c.len() % 2 == 0 && c.len() / 2 > 2));
        }
    }

    #[test]
    fn missing_and_junk_files_are_none() {
        assert!(get("").is_none());
        assert!(get("/definitely/not/here.dcp").is_none());
        let dir = std::env::temp_dir().join(format!("lc-negative-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let junk = dir.join("junk.dcp");
        std::fs::write(&junk, b"not a dcp").unwrap();
        assert!(get(junk.to_str().unwrap()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_files_are_classified_by_name() {
        let f = ProfileFile::from_path(Path::new("/x/Negative Lab - Sony ILCE-7M4 v2.3.dcp")).unwrap();
        assert_eq!(f.camera, "Negative Lab - Sony ILCE-7M4");
        assert_eq!(f.generation, Some(crate::dcp::Generation::V23));
        let f = ProfileFile::from_path(Path::new("/x/Negative Lab - Canon EOS 100D v2.dcp")).unwrap();
        assert_eq!(f.camera, "Negative Lab - Canon EOS 100D");
        assert_eq!(f.generation, Some(crate::dcp::Generation::V2));
        let f = ProfileFile::from_path(Path::new("/x/Negative Lab - Nikon D850.dcp")).unwrap();
        assert_eq!(f.camera, "Negative Lab - Nikon D850");
        assert_eq!(f.generation, Some(crate::dcp::Generation::Base));
        assert!(ProfileFile::from_path(Path::new("/x/notes.txt")).is_none());
    }

    #[test]
    fn discovery_walks_the_tree_and_the_camera_pick_prefers_v2_3() {
        let dir = std::env::temp_dir().join(format!("lc-negative-disc-{}", std::process::id()));
        let sub = dir.join("NegativeLab Camera Profiles");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&sub).unwrap();
        for n in [
            "Negative Lab - Sony ILCE-7M4.dcp",
            "Negative Lab - Sony ILCE-7M4 v2.dcp",
            "Negative Lab - Sony ILCE-7M4 v2.3.dcp",
            "Negative Lab - Canon EOS 100D v2.3.dcp",
        ] {
            std::fs::write(sub.join(n), b"x").unwrap();
        }
        let files = discover(&dir);
        assert_eq!(files.len(), 4, "{files:?}");
        let pick = find_for_models(&files, &["SONY".into(), "ILCE-7M4".into()]).unwrap();
        assert_eq!(pick.name, "Negative Lab - Sony ILCE-7M4 v2.3.dcp");
        assert!(find_for_models(&files, &["Nikon Z8".into()]).is_none());
        assert!(find_for_models(&files, &[]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

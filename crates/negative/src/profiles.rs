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

/// Parse and validate a profile from bytes (`origin` names it in errors).
pub fn parse(data: &[u8], origin: &str) -> Result<Dcp, String> {
    let dcp = Dcp::parse(data).map_err(|e| format!("{origin}: {e}"))?;
    // A profile with no colour matrix cannot drive the colour half; reject it so
    // the caller can report why instead of silently skipping.
    if dcp.color_matrix1.is_none() {
        return Err(format!("{origin}: no ColorMatrix1 tag"));
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

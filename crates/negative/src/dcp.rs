//! Reader for Adobe DNG Camera Profiles (`.dcp`), the format of the per-camera
//! colour profiles a negative-conversion profile is supplied in.
//!
//! A `.dcp` is a classic little-endian TIFF whose header is `IIRC` instead of
//! `II*`. This module reads IFD0 and extracts the DNG camera-profile tags that
//! matter for the conversion, per the public Adobe DNG 1.4/1.6 specification.
//!
//! The large `ProfileLookTableData` payload is kept as raw little-endian `f32`
//! bytes in [`crate::looktable::LookTableData`] and decoded on demand; the tag's
//! byte length is checked against `ProfileLookTableDims` when the profile is
//! parsed. Profiles are supplied by the user (LightCraft ships none).

use crate::color::Matrix;
use crate::looktable::LookTableData;
use std::fmt;

// DNG camera-profile tag numbers (Adobe DNG 1.4/1.6).
pub const TAG_UNIQUE_CAMERA_MODEL: u16 = 0xC614;
pub const TAG_COLOR_MATRIX1: u16 = 0xC621;
pub const TAG_COLOR_MATRIX2: u16 = 0xC622;
pub const TAG_CALIBRATION_ILLUMINANT1: u16 = 0xC65A;
pub const TAG_CALIBRATION_ILLUMINANT2: u16 = 0xC65B;
pub const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 0xC6F4;
pub const TAG_PROFILE_NAME: u16 = 0xC6F8;
pub const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 0xC6F9;
pub const TAG_PROFILE_HUE_SAT_MAP_DATA1: u16 = 0xC6FA;
pub const TAG_PROFILE_HUE_SAT_MAP_DATA2: u16 = 0xC6FB;
pub const TAG_PROFILE_TONE_CURVE: u16 = 0xC6FC;
pub const TAG_PROFILE_EMBED_POLICY: u16 = 0xC6FD;
pub const TAG_PROFILE_COPYRIGHT: u16 = 0xC6FE;
pub const TAG_FORWARD_MATRIX1: u16 = 0xC714;
pub const TAG_FORWARD_MATRIX2: u16 = 0xC715;
pub const TAG_PROFILE_LOOK_TABLE_DIMS: u16 = 0xC725;
pub const TAG_PROFILE_LOOK_TABLE_DATA: u16 = 0xC726;
pub const TAG_PROFILE_LOOK_TABLE_ENCODING: u16 = 0xC7A4;
pub const TAG_PROFILE_HUE_SAT_MAP_ENCODING: u16 = 0xC7A3;
pub const TAG_BASELINE_EXPOSURE_OFFSET: u16 = 0xC7A5;
pub const TAG_DEFAULT_BLACK_RENDER: u16 = 0xC7A6;

/// Errors from parsing a `.dcp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DcpError {
    /// The 4-byte magic was not `IIRC` / `MMRC`.
    NotDcp([u8; 4]),
    /// The file ended before a field could be read.
    Truncated { at: usize, need: usize },
    /// A TIFF field used a type this reader does not know.
    UnsupportedFieldType(u16),
    /// An inline numeric field had the wrong element count for its tag.
    BadCount { tag: u16, count: u32, want: &'static str },
}

impl fmt::Display for DcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DcpError::NotDcp(m) => write!(f, "not a DCP (magic {m:?})"),
            DcpError::Truncated { at, need } => write!(f, "truncated: need {need} bytes at offset {at}"),
            DcpError::UnsupportedFieldType(t) => write!(f, "unsupported TIFF field type {t}"),
            DcpError::BadCount { tag, count, want } => write!(f, "tag 0x{tag:04X} has {count} values, want {want}"),
        }
    }
}

impl std::error::Error for DcpError {}

/// The three profile generations a negative-conversion profile is published in.
///
/// The generation is fixed by the `ProfileName` tag, one of three exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Generation {
    /// `ProfileName = "Negative Lab"`.
    Base,
    /// `ProfileName = "Negative Lab v2.0"`.
    V2,
    /// `ProfileName = "Negative Lab v2.3"`.
    V23,
}

impl Generation {
    /// Exact `ProfileName` string for this generation.
    pub fn profile_name(self) -> &'static str {
        match self {
            Generation::Base => "Negative Lab",
            Generation::V2 => "Negative Lab v2.0",
            Generation::V23 => "Negative Lab v2.3",
        }
    }

    /// Short label (`base` / `v2` / `v2.3`).
    pub fn label(self) -> &'static str {
        match self {
            Generation::Base => "base",
            Generation::V2 => "v2",
            Generation::V23 => "v2.3",
        }
    }

    /// Map an exact `ProfileName` to a generation.
    pub fn from_profile_name(name: &str) -> Option<Generation> {
        match name {
            "Negative Lab" => Some(Generation::Base),
            "Negative Lab v2.0" => Some(Generation::V2),
            "Negative Lab v2.3" => Some(Generation::V23),
            _ => None,
        }
    }

    /// Fall back to the filename suffix (`" v2.3.dcp"`, `" v2.dcp"`).
    pub fn from_filename(name: &str) -> Option<Generation> {
        if name.ends_with(" v2.3.dcp") {
            Some(Generation::V23)
        } else if name.ends_with(" v2.dcp") {
            Some(Generation::V2)
        } else if name.ends_with(".dcp") {
            Some(Generation::Base)
        } else {
            None
        }
    }
}

/// A parsed `.dcp`: the camera-profile tags that matter for the conversion.
///
/// Every field is optional because the tags are optional in the DNG spec and a
/// profile set is not uniform (some profiles carry no LookTable, some v2.3
/// profiles carry a ForwardMatrix, and so on).
#[derive(Debug, Clone)]
pub struct Dcp {
    pub unique_camera_model: Option<String>,
    pub profile_name: Option<String>,
    pub profile_copyright: Option<String>,
    pub profile_calibration_signature: Option<String>,
    /// Row-major 3xN (usually 3x3): XYZ(D50) -> reference camera, under illuminant 1.
    pub color_matrix1: Option<Matrix>,
    pub color_matrix2: Option<Matrix>,
    pub calibration_illuminant1: Option<u16>,
    pub calibration_illuminant2: Option<u16>,
    /// Row-major 3xN (usually 3x3): white-balanced camera -> XYZ(D50).
    pub forward_matrix1: Option<Matrix>,
    pub forward_matrix2: Option<Matrix>,
    pub look_table: Option<LookTableData>,
    /// `ProfileHueSatMapData1` (with `ProfileHueSatMapDims`): the per-camera calibration.
    pub hue_sat_map1: Option<LookTableData>,
    /// `ProfileHueSatMapData2`: the second illuminant's map, when present.
    pub hue_sat_map2: Option<LookTableData>,
    /// Flat `(input, output)` pairs; always even length when present.
    pub profile_tone_curve: Option<Vec<f64>>,
    pub baseline_exposure_offset: Option<f64>,
    pub default_black_render: Option<u32>,
    pub look_table_encoding: Option<u32>,
    pub hue_sat_map_encoding: Option<u32>,
    pub profile_embed_policy: Option<u32>,
}

impl Dcp {
    /// Parse a `.dcp` from memory.
    pub fn parse(data: &[u8]) -> Result<Dcp, DcpError> {
        let e = Endian::of(data)?;
        let ifd_off = e.u32(data, 4)? as usize;

        if ifd_off + 2 > data.len() {
            return Err(DcpError::Truncated { at: ifd_off, need: 2 });
        }
        let n = e.u16(data, ifd_off)? as usize;

        let mut entries: Vec<Entry> = Vec::with_capacity(n.min(1024));
        for i in 0..n {
            let off = ifd_off + 2 + 12 * i;
            let tag = e.u16(data, off)?;
            let typ = e.u16(data, off + 2)?;
            let count = e.u32(data, off + 4)?;
            let (start, end) = field_range(data, e, off, typ, count)?;
            entries.push(Entry { tag, typ, count, start, end });
        }

        let mut dcp = Dcp {
            unique_camera_model: None,
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            color_matrix1: None,
            color_matrix2: None,
            calibration_illuminant1: None,
            calibration_illuminant2: None,
            forward_matrix1: None,
            forward_matrix2: None,
            look_table: None,
            hue_sat_map1: None,
            hue_sat_map2: None,
            profile_tone_curve: None,
            baseline_exposure_offset: None,
            default_black_render: None,
            look_table_encoding: None,
            hue_sat_map_encoding: None,
            profile_embed_policy: None,
        };

        let mut look_dims: Option<[u32; 3]> = None;
        let mut look_bytes: Option<Vec<u8>> = None;
        let mut hue_dims: Option<[u32; 3]> = None;
        let mut hue_bytes1: Option<Vec<u8>> = None;
        let mut hue_bytes2: Option<Vec<u8>> = None;

        for en in &entries {
            let bytes = &data[en.start..en.end];
            match en.tag {
                TAG_UNIQUE_CAMERA_MODEL => dcp.unique_camera_model = Some(ascii(bytes)),
                TAG_PROFILE_NAME => dcp.profile_name = Some(ascii(bytes)),
                TAG_PROFILE_COPYRIGHT => dcp.profile_copyright = Some(ascii(bytes)),
                TAG_PROFILE_CALIBRATION_SIGNATURE => dcp.profile_calibration_signature = Some(ascii(bytes)),
                TAG_COLOR_MATRIX1 => dcp.color_matrix1 = Some(matrix(en, bytes, e)?),
                TAG_COLOR_MATRIX2 => dcp.color_matrix2 = Some(matrix(en, bytes, e)?),
                TAG_FORWARD_MATRIX1 => dcp.forward_matrix1 = Some(matrix(en, bytes, e)?),
                TAG_FORWARD_MATRIX2 => dcp.forward_matrix2 = Some(matrix(en, bytes, e)?),
                TAG_CALIBRATION_ILLUMINANT1 => dcp.calibration_illuminant1 = Some(e.u16(bytes, 0)?),
                TAG_CALIBRATION_ILLUMINANT2 => dcp.calibration_illuminant2 = Some(e.u16(bytes, 0)?),
                TAG_PROFILE_TONE_CURVE => dcp.profile_tone_curve = Some(f64_values(en, bytes, e)?),
                TAG_PROFILE_LOOK_TABLE_DIMS => look_dims = Some(dims3(en, bytes, e)?),
                TAG_PROFILE_HUE_SAT_MAP_DIMS => hue_dims = Some(dims3(en, bytes, e)?),
                TAG_PROFILE_LOOK_TABLE_DATA => look_bytes = Some(bytes.to_vec()),
                TAG_PROFILE_HUE_SAT_MAP_DATA1 => hue_bytes1 = Some(bytes.to_vec()),
                TAG_PROFILE_HUE_SAT_MAP_DATA2 => hue_bytes2 = Some(bytes.to_vec()),
                TAG_PROFILE_LOOK_TABLE_ENCODING => dcp.look_table_encoding = Some(e.u32(bytes, 0)?),
                TAG_PROFILE_HUE_SAT_MAP_ENCODING => dcp.hue_sat_map_encoding = Some(e.u32(bytes, 0)?),
                TAG_BASELINE_EXPOSURE_OFFSET => dcp.baseline_exposure_offset = Some(f64_values(en, bytes, e)?.first().copied().unwrap_or(0.0)),
                TAG_DEFAULT_BLACK_RENDER => dcp.default_black_render = Some(e.u32(bytes, 0)?),
                TAG_PROFILE_EMBED_POLICY => dcp.profile_embed_policy = Some(e.u32(bytes, 0)?),
                _ => {}
            }
        }

        if let Some(dims) = look_dims {
            dcp.look_table = Some(assemble_table(dims, look_bytes, dcp.look_table_encoding, TAG_PROFILE_LOOK_TABLE_DATA)?);
        }
        if let Some(dims) = hue_dims {
            dcp.hue_sat_map1 = Some(assemble_table(dims, hue_bytes1, dcp.hue_sat_map_encoding, TAG_PROFILE_HUE_SAT_MAP_DATA1)?);
            if let Some(bytes) = hue_bytes2 {
                dcp.hue_sat_map2 = Some(assemble_table(dims, Some(bytes), dcp.hue_sat_map_encoding, TAG_PROFILE_HUE_SAT_MAP_DATA2)?);
            }
        }

        Ok(dcp)
    }

    /// Read and parse a `.dcp` file.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> std::io::Result<Dcp> {
        let data = std::fs::read(path)?;
        Dcp::parse(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// The profile generation, from `ProfileName` where possible.
    pub fn generation(&self) -> Option<Generation> {
        self.profile_name.as_deref().and_then(Generation::from_profile_name)
    }

    /// Camera model: `UniqueCameraModel`, or `None`.
    pub fn camera_model(&self) -> Option<&str> {
        self.unique_camera_model.as_deref()
    }
}

/// One decoded IFD entry with the byte range of its value.
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn of(data: &[u8]) -> Result<Endian, DcpError> {
        if data.len() < 4 {
            let mut m = [0u8; 4];
            m[..data.len()].copy_from_slice(data);
            return Err(DcpError::NotDcp(m));
        }
        match &data[0..4] {
            b"IIRC" => Ok(Endian::Little),
            b"MMRC" => Ok(Endian::Big),
            _ => Err(DcpError::NotDcp([data[0], data[1], data[2], data[3]])),
        }
    }

    fn u16(self, d: &[u8], at: usize) -> Result<u16, DcpError> {
        let b = slice(d, at, 2)?;
        Ok(match self {
            Endian::Little => u16::from_le_bytes([b[0], b[1]]),
            Endian::Big => u16::from_be_bytes([b[0], b[1]]),
        })
    }

    fn u32(self, d: &[u8], at: usize) -> Result<u32, DcpError> {
        let b = slice(d, at, 4)?;
        Ok(match self {
            Endian::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Endian::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        })
    }

    fn i32(self, d: &[u8], at: usize) -> Result<i32, DcpError> {
        Ok(self.u32(d, at)? as i32)
    }

    fn f32(self, d: &[u8], at: usize) -> Result<f32, DcpError> {
        let b = slice(d, at, 4)?;
        Ok(match self {
            Endian::Little => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Endian::Big => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        })
    }

    fn f64(self, d: &[u8], at: usize) -> Result<f64, DcpError> {
        let b = slice(d, at, 8)?;
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(match self {
            Endian::Little => f64::from_le_bytes(a),
            Endian::Big => f64::from_be_bytes(a),
        })
    }
}

fn slice(d: &[u8], at: usize, n: usize) -> Result<&[u8], DcpError> {
    if at.checked_add(n).is_none_or(|end| end > d.len()) {
        return Err(DcpError::Truncated { at, need: n });
    }
    Ok(&d[at..at + n])
}

/// Bytes per element of a TIFF field type.
fn type_size(typ: u16) -> Result<usize, DcpError> {
    Ok(match typ {
        1 | 2 | 6 | 7 => 1, // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,         // SHORT, SSHORT
        4 | 9 | 11 => 4,    // LONG, SLONG, FLOAT
        5 | 10 | 12 => 8,   // RATIONAL, SRATIONAL, DOUBLE
        other => return Err(DcpError::UnsupportedFieldType(other)),
    })
}

/// Resolve the byte range holding a field's value (inline or off to the side).
fn field_range(data: &[u8], e: Endian, entry_off: usize, typ: u16, count: u32) -> Result<(usize, usize), DcpError> {
    let per = type_size(typ)?;
    let size = per.checked_mul(count as usize).ok_or(DcpError::Truncated { at: entry_off, need: per })?;
    if size <= 4 {
        let start = entry_off + 8;
        slice(data, start, size)?;
        Ok((start, start + size))
    } else {
        let off = e.u32(data, entry_off + 8)? as usize;
        slice(data, off, size)?;
        Ok((off, off + size))
    }
}

fn ascii(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn f64_values(en: &Entry, bytes: &[u8], e: Endian) -> Result<Vec<f64>, DcpError> {
    let n = en.count as usize;
    let per = type_size(en.typ)?;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for i in 0..n {
        let at = i * per;
        let v = match en.typ {
            1 | 7 => bytes[at] as f64,            // BYTE / UNDEFINED
            6 => bytes[at] as i8 as f64,          // SBYTE
            3 => e.u16(bytes, at)? as f64,        // SHORT
            8 => e.u16(bytes, at)? as i16 as f64, // SSHORT
            4 => e.u32(bytes, at)? as f64,        // LONG
            9 => e.i32(bytes, at)? as f64,        // SLONG
            11 => e.f32(bytes, at)? as f64,       // FLOAT
            12 => e.f64(bytes, at)?,              // DOUBLE
            5 => {
                // RATIONAL
                let num = e.u32(bytes, at)? as f64;
                let den = e.u32(bytes, at + 4)? as f64;
                num / den
            }
            10 => {
                // SRATIONAL
                let num = e.i32(bytes, at)? as f64;
                let den = e.i32(bytes, at + 4)? as f64;
                num / den
            }
            other => return Err(DcpError::UnsupportedFieldType(other)),
        };
        out.push(v);
    }
    Ok(out)
}

fn u32_values(en: &Entry, bytes: &[u8], e: Endian) -> Result<Vec<u32>, DcpError> {
    let n = en.count as usize;
    let per = type_size(en.typ)?;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for i in 0..n {
        let at = i * per;
        let v = match en.typ {
            1 | 7 => bytes[at] as u32,
            3 => e.u16(bytes, at)? as u32,
            4 => e.u32(bytes, at)?,
            other => return Err(DcpError::UnsupportedFieldType(other)),
        };
        out.push(v);
    }
    Ok(out)
}

fn dims3(en: &Entry, bytes: &[u8], e: Endian) -> Result<[u32; 3], DcpError> {
    let dims = u32_values(en, bytes, e)?;
    if dims.len() != 3 {
        return Err(DcpError::BadCount { tag: en.tag, count: en.count, want: "3" });
    }
    Ok([dims[0], dims[1], dims[2]])
}

/// Assemble and validate a raw hue/sat table payload.
fn assemble_table(dims: [u32; 3], bytes: Option<Vec<u8>>, encoding: Option<u32>, tag: u16) -> Result<LookTableData, DcpError> {
    let table = LookTableData { dims, encoding, le_bytes: bytes.unwrap_or_default() };
    table.validate().map_err(|_| DcpError::BadCount { tag, count: table.le_bytes.len() as u32, want: "dims*3*4" })?;
    Ok(table)
}

fn matrix(en: &Entry, bytes: &[u8], e: Endian) -> Result<Matrix, DcpError> {
    let v = f64_values(en, bytes, e)?;
    Matrix::from_rows3(v).ok_or(DcpError::BadCount { tag: en.tag, count: en.count, want: "3*N" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_names_are_exact() {
        for (name, g) in [("Negative Lab", Generation::Base), ("Negative Lab v2.0", Generation::V2), ("Negative Lab v2.3", Generation::V23)] {
            assert_eq!(Generation::from_profile_name(name), Some(g));
            assert_eq!(g.profile_name(), name);
        }
        assert_eq!(Generation::from_profile_name("Negative Lab v2"), None);
        assert_eq!(Generation::from_profile_name("negative lab"), None);
    }

    #[test]
    fn rejects_non_dcp() {
        assert!(matches!(Dcp::parse(b"II*\0\x08\0\0\0"), Err(DcpError::NotDcp(m)) if m == *b"II*\0"));
        assert!(matches!(Dcp::parse(b"ab"), Err(DcpError::NotDcp(_))));
    }
}

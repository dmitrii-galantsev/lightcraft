//! The conversion's parameters: the film-base state, the modelled preset
//! parameters, and the profile/space options.
//!
//! These mirror the parameter vocabulary of the negative-conversion presets
//! (tone profile, colour density, clip, region colour, fade/glow, LUT,
//! process order). The numeric encodings are what the LightCraft develop
//! controls store, so a slider can address each one by index.

use crate::color::D50_XY;
use crate::dcp::Dcp;
use std::sync::Arc;

/// Film density model: `addDensity` raises contrast, `neutralDensity` is the
/// identity. `AddDensity` is the shipped default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDensity {
    #[default]
    AddDensity,
    NeutralDensity,
}

impl ColorDensity {
    pub fn code(self) -> u8 {
        match self {
            ColorDensity::AddDensity => 0,
            ColorDensity::NeutralDensity => 1,
        }
    }
    pub fn from_code(v: u8) -> ColorDensity {
        match v {
            1 => ColorDensity::NeutralDensity,
            _ => ColorDensity::AddDensity,
        }
    }
}

/// Tone-curve shape family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToneProfile {
    #[default]
    Standard,
    Linear,
    LinearGamma,
    Logarithmic,
}

impl ToneProfile {
    pub fn code(self) -> u8 {
        match self {
            ToneProfile::Standard => 0,
            ToneProfile::Linear => 1,
            ToneProfile::LinearGamma => 2,
            ToneProfile::Logarithmic => 3,
        }
    }
    pub fn from_code(v: u8) -> ToneProfile {
        match v {
            1 => ToneProfile::Linear,
            2 => ToneProfile::LinearGamma,
            3 => ToneProfile::Logarithmic,
            _ => ToneProfile::Standard,
        }
    }
}

/// Whether colour is applied before or after tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessOrder {
    #[default]
    ColorFirst,
    ToneFirst,
}

impl ProcessOrder {
    pub fn code(self) -> u8 {
        match self {
            ProcessOrder::ColorFirst => 0,
            ProcessOrder::ToneFirst => 1,
        }
    }
    pub fn from_code(v: u8) -> ProcessOrder {
        match v {
            1 => ProcessOrder::ToneFirst,
            _ => ProcessOrder::ColorFirst,
        }
    }
}

/// Named paper/film look LUT. `Natural` is the identity; the other three are
/// small per-channel gains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lut {
    Crystal,
    Frontier,
    #[default]
    Natural,
    Pakon,
}

impl Lut {
    pub fn code(self) -> u8 {
        match self {
            Lut::Crystal => 0,
            Lut::Frontier => 1,
            Lut::Natural => 2,
            Lut::Pakon => 3,
        }
    }
    pub fn from_code(v: u8) -> Lut {
        match v {
            0 => Lut::Crystal,
            1 => Lut::Frontier,
            3 => Lut::Pakon,
            _ => Lut::Natural,
        }
    }
}

/// The modelled preset parameters that shape the positive after the invert.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresetParams {
    /// Saturation amount (`-100..100`, percent); `0` is neutral.
    pub saturation: f32,
    pub color_density: ColorDensity,
    /// Negative percentages, as in the preset vocabulary.
    pub black_clip: f32,
    pub white_clip: f32,
    pub tone_profile: ToneProfile,
    pub lights: f32,
    pub darks: f32,
    pub whites: f32,
    pub blacks: f32,
    pub lab_fade: f32,
    pub lab_glow: f32,
    pub shadows_range: f32,
    pub highs_range: f32,
    /// Region colour deltas: `[shadows, mids, highs]` per axis.
    pub red_cyan: [f32; 3],
    pub green_magenta: [f32; 3],
    pub blue_yellow: [f32; 3],
    pub process_order: ProcessOrder,
    pub lut: Lut,
}

impl Default for PresetParams {
    fn default() -> Self {
        PresetParams {
            saturation: 0.0,
            color_density: ColorDensity::AddDensity,
            black_clip: 0.0,
            white_clip: 0.0,
            tone_profile: ToneProfile::Linear,
            lights: 0.0,
            darks: 0.0,
            whites: 0.0,
            blacks: 0.0,
            lab_fade: 0.0,
            lab_glow: 0.0,
            shadows_range: 5.0,
            highs_range: 5.0,
            red_cyan: [0.0; 3],
            green_magenta: [0.0; 3],
            blue_yellow: [0.0; 3],
            process_order: ProcessOrder::ColorFirst,
            lut: Lut::Natural,
        }
    }
}

/// The working space the profile is rendered into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkingSpace {
    #[default]
    ProPhoto,
    Srgb,
}

impl WorkingSpace {
    /// XYZ(D50) -> linear working-space matrix.
    pub fn xyz_d50_to_working(self) -> crate::color::Mat3 {
        match self {
            WorkingSpace::ProPhoto => crate::color::xyz_d50_to_prophoto(),
            WorkingSpace::Srgb => crate::color::xyz_d50_to_srgb(),
        }
    }
}

/// Everything the conversion needs to run.
#[derive(Clone)]
pub struct Params {
    /// Per-channel film base (the invert window's white end), encoded `0..1`.
    pub base: [f32; 3],
    /// Per-channel dark endpoint (the invert window's black end), encoded `0..1`.
    /// **An explicit, documented parameter.** The rule by which the real plugin
    /// derives it from the image is unknown and content-dependent
    /// (`KNOWN-GAPS.md` G23); this crate does not invent one.
    pub dark: [f32; 3],
    pub preset: PresetParams,
    /// Gamma the scan is stored with. `1.0` = already encoded (the confirmed
    /// window is a function of the encoded values).
    pub scan_gamma: f32,
    /// Gamma to encode the result with. `1.0` keeps it encoded.
    pub output_gamma: f32,
    /// The shot white chromaticity for the profile chain.
    pub white_xy: [f64; 2],
    pub working_space: WorkingSpace,
    /// The camera profile to apply. `None` skips the profile half.
    pub profile: Option<Arc<Dcp>>,
    /// Profile blend amount, `0..1` (a linear blend toward the un-profiled image).
    pub profile_amount: f32,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            base: [1.0; 3],
            dark: [0.0; 3],
            preset: PresetParams::default(),
            scan_gamma: 1.0,
            output_gamma: 1.0,
            white_xy: D50_XY,
            working_space: WorkingSpace::ProPhoto,
            profile: None,
            profile_amount: 1.0,
        }
    }
}

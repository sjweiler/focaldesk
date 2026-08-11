//! ICC profile parsing and display-profile discovery (Phase B).

use crate::core::color::{
    ColorDescription, ColorPrimaries, PrimariesChromaticity, TransferFunction,
};
use crate::core::icc_lut::{self, OutputIccLut};
use lcms2::{ColorSpaceSignature, Profile, ProfileClassSignature, Tag, TagSignature, ToneCurveRef};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};

pub const MAX_ICC_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum IccError {
    Invalid(&'static str),
    Io(std::io::Error),
    Parse(lcms2::Error),
}

impl From<std::io::Error> for IccError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<lcms2::Error> for IccError {
    fn from(value: lcms2::Error) -> Self {
        Self::Parse(value)
    }
}

#[derive(Clone, Debug)]
pub struct ParsedIccProfile {
    pub description: ColorDescription,
    pub bytes: Vec<u8>,
    /// Baked sRGB → device 3D LUT (C2c); used when present instead of parametric encode.
    pub output_lut: Option<OutputIccLut>,
}

/// Read `length` bytes at `offset` from a client-supplied seekable fd.
pub fn read_icc_from_fd(
    icc_profile: OwnedFd,
    offset: u32,
    length: u32,
) -> Result<Vec<u8>, IccError> {
    let length = length as usize;
    if length == 0 || length > MAX_ICC_BYTES {
        return Err(IccError::Invalid("bad size"));
    }
    let mut file = File::from(icc_profile);
    let len = file.seek(SeekFrom::End(0))?;
    if u64::from(offset) + length as u64 > len {
        return Err(IccError::Invalid("out of file"));
    }
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut data = vec![0u8; length];
    file.read_exact(&mut data)?;
    Ok(data)
}

/// Parse an ICC v2/v4 RGB display or color-space profile into a compositor description.
pub fn parse_icc_profile(data: &[u8]) -> Result<ParsedIccProfile, IccError> {
    if data.is_empty() || data.len() > MAX_ICC_BYTES {
        return Err(IccError::Invalid("bad size"));
    }

    let profile = Profile::new_icc(data)?;
    validate_icc_profile(&profile)?;

    let primaries = primaries_from_profile(&profile)?;
    let transfer = transfer_from_profile(&profile);
    let (reference_white_nits, max_luminance_nits) = luminances_from_profile(&profile);
    let description = ColorDescription {
        primaries,
        transfer,
        reference_white_nits,
        max_luminance_nits,
        max_cll_nits: None,
        max_fall_nits: None,
    };
    let output_lut = icc_lut::build_output_to_device_lut(data, description).ok();

    Ok(ParsedIccProfile {
        description,
        bytes: data.to_vec(),
        output_lut,
    })
}

pub fn load_display_profile_from_path(path: &Path) -> Result<ParsedIccProfile, IccError> {
    let bytes = std::fs::read(path)?;
    parse_icc_profile(&bytes)
}

fn validate_icc_profile(profile: &Profile) -> Result<(), IccError> {
    let major = profile.version().floor() as u32;
    if major != 2 && major != 4 {
        return Err(IccError::Invalid("unsupported ICC version"));
    }

    if profile.color_space() != ColorSpaceSignature::RgbData {
        return Err(IccError::Invalid("not an RGB profile"));
    }

    match profile.device_class() {
        ProfileClassSignature::DisplayClass | ProfileClassSignature::ColorSpaceClass => Ok(()),
        _ => Err(IccError::Invalid("unsupported ICC device class")),
    }
}

fn primaries_from_profile(profile: &Profile) -> Result<ColorPrimaries, IccError> {
    let r = colorant_xy(profile, TagSignature::RedColorantTag)?;
    let g = colorant_xy(profile, TagSignature::GreenColorantTag)?;
    let b = colorant_xy(profile, TagSignature::BlueColorantTag)?;
    let w = colorant_xy(profile, TagSignature::MediaWhitePointTag)?;

    if let Some(named) = match_named_primaries(r, g, b, w) {
        return Ok(named);
    }

    Ok(ColorPrimaries::Custom(PrimariesChromaticity { r, g, b, w }))
}

fn colorant_xy(profile: &Profile, tag: TagSignature) -> Result<[f32; 2], IccError> {
    let Tag::CIEXYZ(xyz) = profile.read_tag(tag) else {
        return Err(IccError::Invalid("missing ICC colorant tag"));
    };
    Ok(xyz_to_xy(xyz.X, xyz.Y, xyz.Z))
}

fn xyz_to_xy(x: f64, y: f64, z: f64) -> [f32; 2] {
    let sum = x + y + z;
    if sum <= 0.0 {
        return [0.3127, 0.3290];
    }
    [(x / sum) as f32, (y / sum) as f32]
}

fn match_named_primaries(
    r: [f32; 2],
    g: [f32; 2],
    b: [f32; 2],
    w: [f32; 2],
) -> Option<ColorPrimaries> {
    const TOL: f32 = 0.02;
    let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() <= TOL && (a[1] - b[1]).abs() <= TOL;

    let srgb = PrimariesChromaticity::SRGB;
    let p3 = PrimariesChromaticity::DISPLAY_P3;
    let bt2020 = PrimariesChromaticity::BT2020;

    if close(r, srgb.r) && close(g, srgb.g) && close(b, srgb.b) && close(w, srgb.w) {
        return Some(ColorPrimaries::Srgb);
    }
    if close(r, p3.r) && close(g, p3.g) && close(b, p3.b) && close(w, p3.w) {
        return Some(ColorPrimaries::DisplayP3);
    }
    if close(r, bt2020.r) && close(g, bt2020.g) && close(b, bt2020.b) && close(w, bt2020.w) {
        return Some(ColorPrimaries::Bt2020);
    }
    None
}

fn transfer_from_profile(profile: &Profile) -> TransferFunction {
    let Tag::ToneCurve(curve) = profile.read_tag(TagSignature::RedTRCTag) else {
        return TransferFunction::Srgb;
    };
    classify_trc(curve)
}

fn classify_trc(curve: &ToneCurveRef) -> TransferFunction {
    if curve.is_linear() {
        return TransferFunction::Linear;
    }

    match curve.parametric_type() {
        4 => TransferFunction::Srgb,
        1 => {
            if curve
                .estimated_gamma(0.05)
                .is_some_and(|g| (g - 2.2).abs() < 0.12)
            {
                TransferFunction::Gamma22
            } else {
                TransferFunction::Srgb
            }
        }
        _ => {
            if let Some(g) = curve.estimated_gamma(0.05) {
                if (g - 1.0).abs() < 0.02 {
                    TransferFunction::Linear
                } else if (g - 2.2).abs() < 0.12 {
                    TransferFunction::Gamma22
                } else {
                    TransferFunction::Srgb
                }
            } else {
                TransferFunction::Srgb
            }
        }
    }
}

fn luminances_from_profile(profile: &Profile) -> (f32, f32) {
    let max_nits = match profile.read_tag(TagSignature::LuminanceTag) {
        Tag::CIEXYZ(xyz) if xyz.Y > 1.0 => xyz.Y as f32,
        _ => 80.0,
    };
    (80.0, max_nits.max(80.0))
}

fn chromaticity_is_valid(ch: &PrimariesChromaticity) -> bool {
    let in_range = |xy: [f32; 2]| {
        xy[0].is_finite()
            && xy[1].is_finite()
            && (0.0..=1.0).contains(&xy[0])
            && (0.0..=1.0).contains(&xy[1])
    };
    in_range(ch.r) && in_range(ch.g) && in_range(ch.b) && in_range(ch.w)
}

/// Build an sRGB-ish description from EDID base-block chromaticities (fallback).
pub fn color_description_from_edid(edid: &[u8]) -> Option<ColorDescription> {
    if edid.len() < 128
        || edid.get(0..8) != Some([0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0].as_slice())
    {
        return None;
    }

    let chroma = |high: u8, low: u8| -> f32 {
        let raw = (u16::from(high) << 2) | u16::from(low);
        f32::from(raw) / 1024.0
    };
    let chroma_low_1 = edid[25];
    let chroma_low_2 = edid[26];

    let primaries = PrimariesChromaticity {
        r: [
            chroma(edid[27], (chroma_low_1 >> 6) & 0x03),
            chroma(edid[28], (chroma_low_1 >> 4) & 0x03),
        ],
        g: [
            chroma(edid[29], (chroma_low_1 >> 2) & 0x03),
            chroma(edid[30], chroma_low_1 & 0x03),
        ],
        b: [
            chroma(edid[31], (chroma_low_2 >> 6) & 0x03),
            chroma(edid[32], (chroma_low_2 >> 4) & 0x03),
        ],
        w: [
            chroma(edid[33], (chroma_low_2 >> 2) & 0x03),
            chroma(edid[34], chroma_low_2 & 0x03),
        ],
    };

    Some(ColorDescription {
        primaries: if chromaticity_is_valid(&primaries) {
            ColorPrimaries::Custom(primaries)
        } else {
            ColorPrimaries::Srgb
        },
        transfer: TransferFunction::Srgb,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
    })
}

/// MD5 hex of the full EDID blob (colord `edid-{hash}.icc` naming).
pub fn edid_md5_hex(edid: &[u8]) -> String {
    format!("{:x}", md5::compute(edid))
}

/// Load `edid-{md5}.icc` from the standard ICC search dirs (GNOME/KDE generated).
pub fn load_display_profile_by_edid_hash(edid: &[u8]) -> Option<ParsedIccProfile> {
    if edid.is_empty() {
        return None;
    }
    let name = format!("edid-{}.icc", edid_md5_hex(edid));
    for dir in icc_search_dirs() {
        let path = dir.join(&name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(parsed) = parse_icc_profile(&bytes) {
            return Some(parsed);
        }
    }
    None
}

/// Locate a display ICC profile on disk for the given monitor identity.
pub fn load_display_profile_for_monitor(
    make: &str,
    model: &str,
    serial: &str,
) -> Option<ParsedIccProfile> {
    let make_l = make.to_ascii_lowercase();
    let model_l = model.to_ascii_lowercase();
    let serial_l = serial.to_ascii_lowercase();

    for dir in icc_search_dirs() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("icc") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(parsed) = parse_icc_profile(&bytes) else {
                continue;
            };
            if profile_matches_monitor(&parsed, &make_l, &model_l, &serial_l) {
                return Some(parsed);
            }
        }
    }
    None
}

fn icc_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/var/lib/colord/icc"),
        PathBuf::from("/usr/share/color/icc"),
        PathBuf::from("/usr/local/share/color/icc"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/icc"));
        dirs.push(home.join(".color/icc"));
    }
    dirs
}

fn profile_matches_monitor(
    parsed: &ParsedIccProfile,
    make: &str,
    model: &str,
    serial: &str,
) -> bool {
    let profile = match Profile::new_icc(&parsed.bytes) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let haystacks = [
        profile.info(lcms2::InfoType::Manufacturer, lcms2::Locale::default()),
        profile.info(lcms2::InfoType::Model, lcms2::Locale::default()),
        profile.info(lcms2::InfoType::Description, lcms2::Locale::default()),
    ];

    let tokens = [make, model, serial]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    let need = tokens.len().min(2);
    if need == 0 {
        return false;
    }

    let matched = tokens
        .iter()
        .filter(|token| {
            haystacks
                .iter()
                .flatten()
                .any(|h| h.to_ascii_lowercase().contains(**token))
        })
        .count();

    matched >= need
}

/// Copy ICC bytes into a sealed read-only memfd for `wp_image_description_info_v1.icc_file`.
pub fn memfd_from_bytes(data: &[u8]) -> Result<OwnedFd, std::io::Error> {
    let name = CString::new("focaldesk-icc")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(data)?;
    file.flush()?;
    Ok(OwnedFd::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::{PrimariesChromaticity, TransferFunction};

    #[test]
    fn parse_lcms_srgb_profile() {
        let profile = Profile::new_srgb();
        let bytes = profile.icc().expect("serialize sRGB ICC");
        let parsed = parse_icc_profile(&bytes).expect("parse sRGB ICC");
        assert_eq!(parsed.description.transfer, TransferFunction::Srgb);
        let ch = parsed.description.primaries.chromaticity();
        let srgb = PrimariesChromaticity::SRGB;
        assert!((ch.r[0] - srgb.r[0]).abs() < 0.05);
        assert!((ch.g[1] - srgb.g[1]).abs() < 0.05);
    }

    #[test]
    fn rejects_empty_icc() {
        assert!(parse_icc_profile(&[]).is_err());
    }

    #[test]
    fn edid_generated_profile_parses_if_present() {
        let path = std::path::Path::new(env!("HOME"))
            .join(".local/share/icc/edid-80cab7f6884553b4890a7fa9c986c84d.icc");
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read edid icc");
        parse_icc_profile(&bytes).expect("parse edid-generated icc");
    }

    #[test]
    fn edid_hash_profile_loads_if_present() {
        let edid_path = std::path::Path::new("/sys/class/drm/card2-DP-4/edid");
        if !edid_path.exists() {
            return;
        }
        let edid = std::fs::read(edid_path).expect("read edid");
        let parsed = load_display_profile_by_edid_hash(&edid).expect("load by hash");
        assert!(!parsed.bytes.is_empty());
    }

    #[test]
    fn edid_md5_hex_is_lowercase_32_chars() {
        let hash = edid_md5_hex(b"edid-bytes");
        assert_eq!(hash.len(), 32);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn edid_chromaticities_produce_custom_primaries() {
        let mut edid = [0_u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        edid[25] = 0x00;
        edid[26] = 0x00;
        edid[27] = 0xa3;
        edid[28] = 0x54;
        edid[29] = 0x4c;
        edid[30] = 0x99;
        edid[31] = 0x26;
        edid[32] = 0x0f;
        edid[33] = 0x50;
        edid[34] = 0x54;

        let desc = color_description_from_edid(&edid).expect("edid color");
        match desc.primaries {
            ColorPrimaries::Custom(ch) => {
                assert!(
                    ch.r[0] > 0.0 && ch.r[0] < 1.0,
                    "red x out of range: {}",
                    ch.r[0]
                );
                assert!(
                    ch.g[1] > 0.0 && ch.g[1] < 1.0,
                    "green y out of range: {}",
                    ch.g[1]
                );
            }
            other => panic!("expected custom primaries, got {other:?}"),
        }
    }
}

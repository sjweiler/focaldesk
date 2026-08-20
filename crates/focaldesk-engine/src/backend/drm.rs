// DRM/KMS backend — uses the same [`DesktopState`] path as winit via [`crate::backend::common::NestedDesktop`].
// Full session/udev/scanout should follow the Smithay anvil `udev` backend pattern.

use crate::backend::common::{
    bootstrap_compositor_core, is_nonfatal_wayland_io_error, physical_size_mm_from_pixels,
    refresh_portal_services, spawn_session_sleep_watch, stop_focaldesk_session_target,
    SessionSleepEvent,
};
use crate::backend::drm::drm::buffer::DrmModifier;
use drm::control::{connector, crtc, property};
use smithay::backend::allocator::Allocator;
use smithay::backend::input::{InputEvent, KeyState, SwitchState, SwitchToggleEvent};
use smithay::reexports::drm::control::Device as _;
use smithay::reexports::input::event::switch::Switch as InputSwitch;
// `DrmOutput::render_frame` / `initialize_output` drive an internal [`smithay::backend::drm::compositor::DrmCompositor`].

use smithay::backend::input::KeyboardKeyEvent;
//use smithay::backend::renderer::element::{Id, Kind};
use crate::core::backend_render::prepare_output;
use crate::core::linear_compositing::{
    run_linear_staged_pass, run_sdr_pass, select_hdr_offscreen_format, supports_linear_sdr,
    use_linear_sdr_path, LinearOffscreenTargets, OffscreenTexture,
};
use smithay::backend::renderer::utils::DamageBag;
//use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    render_elements, texture::TextureRenderElement, Id, Kind,
};

use focaldesk_flow::keybinds::BackendKind;
use focaldesk_logging::flog_warn;
use focaldesk_settings_core::{
    load_exclusive_hdr_state, save_exclusive_hdr_state, DisplayColorProfile, ExclusiveHdrPhase,
    ExclusiveHdrState,
};

// DRM/KMS backend for FocalDesk.
//
// This is the real hardware backend counterpart to the winit backend.
// It keeps compositor state shared, but owns its own session/device/input/output plumbing.
//
// This is intentionally a bring-up skeleton:
// - struct layout is real
// - event loop wiring is real
// - device/output attach points are real
// - many internals are still TODO so you can connect them to your existing FocalDesk code

use anyhow::{anyhow, Context, Result};
use smithay::backend::renderer::Bind;
use smithay::utils::DeviceFd;

use std::{
    collections::HashMap,
    error::Error,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::backend::common::translate_backend_input;
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use focaldesk_logging::flog;
use focaldesk_resources::RenderResources;
use focaldesk_types::OutputId;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::Renderer;

use smithay::{
    backend::{
        allocator::{gbm::GbmAllocator, gbm::GbmBufferFlags, Fourcc, Modifier},
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode},
        egl::{
            self,
            context::{ContextPriority, GlAttributes, PixelFormatRequirements},
            EGLContext,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::solid::SolidColorRenderElement,
            gles::{GlesRenderer, GlesTarget, GlesTexture},
            Color32F, ExportMem, ImportDma, ImportEgl,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{primary_gpu, UdevBackend, UdevEvent},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop,
        gbm::Device as GbmDevice,
        input::Libinput,
        wayland_server::{Client, Display, ListeningSocket},
    },
    utils::{Buffer, IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use smithay::backend::drm::{
    compositor::FrameFlags,
    exporter::gbm::GbmFramebufferExporter,
    output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
    HdrState,
};

use crate::core::chrome_layout::build_chrome_layout;
use crate::core::{
    desktop::{DamageSource, DesktopState},
    ui_state::UiState,
    OutputState, SceneState,
};

use smithay::backend::egl::{EGLDevice, EGLDisplay};

use smithay::reexports::drm;

use drm_ffi;

use smithay::reexports::rustix::fs::OFlags;

use chrono::Local;
use std::fs;

use crate::backend::common::client_state_from_stream;
#[cfg(feature = "xwayland")]
use crate::backend::common::{finish_xwayland_startup, start_xwayland};
use drm::control::Mode;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayConfig {
    pub name: String,
    pub enabled: bool,

    pub mode_width: i32,
    pub mode_height: i32,
    pub refresh_mhz: i32,

    pub scale: f64,

    pub logical_x: i32,
    pub logical_y: i32,

    pub physical_width_mm: Option<i32>,
    pub physical_height_mm: Option<i32>,

    pub primary: bool,
    pub transform: DisplayTransform,

    #[serde(default)]
    pub hdr_supported: bool,
    #[serde(default)]
    pub hdr_requested: bool,
    #[serde(default)]
    pub hdr_enabled: bool,
    #[serde(default)]
    pub color_profile: DisplayColorProfile,
    #[serde(default)]
    pub icc_profile_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HdrSupport {
    pub has_hdr_metadata_property: bool,
    pub has_bt2020_colorspace: bool,
    pub colorspaces: Vec<String>,
    pub current_colorspace: Option<String>,
    pub max_bpc: Option<HdrBpcRange>,
    pub current_max_bpc: Option<u64>,
    pub hdr_metadata_blob: Option<u64>,
    pub edid_hdr_static_metadata: bool,
    pub edid_static_metadata_type1: bool,
    pub edid_pq: bool,
    pub edid_hlg: bool,
    pub edid_hdr_metadata: Option<EdidHdrMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdidHdrMetadata {
    pub display_primaries: [(u16, u16); 3],
    pub white_point: (u16, u16),
    pub max_luminance: u16,
    pub min_luminance: u16,
    pub max_fall: u16,
}

#[derive(Debug, Clone)]
pub struct HdrBpcRange {
    pub min: u64,
    pub max: u64,
}

impl HdrSupport {
    pub(crate) fn is_detected(&self) -> bool {
        self.edid_hdr_static_metadata && self.edid_pq
    }

    /// Whether the connector exposes the controls and EDID data needed to
    /// signal HDR10.  Link depth is checked separately because some drivers
    /// (notably NVIDIA DRM) do not expose a writable `max bpc` property and
    /// instead derive link depth from the active primary-plane format.
    pub(crate) fn can_signal_hdr10(&self) -> bool {
        self.has_hdr_metadata_property
            && self.has_bt2020_colorspace
            && self.edid_hdr_static_metadata
            && self.edid_static_metadata_type1
            && self.edid_pq
            && self.edid_hdr_metadata.is_some()
    }

    pub(crate) fn can_enable(&self, ten_bit_scanout_active: bool) -> bool {
        self.can_signal_hdr10() && ten_bit_scanout_active && self.bpc_control_allows_ten_bit()
    }

    fn has_connector_controls(&self) -> bool {
        self.has_hdr_metadata_property || self.has_bt2020_colorspace
    }

    /// An absent `max bpc` property means the driver manages link depth.  In
    /// that case a successfully initialized 10-bit KMS scanout is the proof we
    /// use.  If the property does exist, its range must include 10 bpc.
    fn bpc_control_allows_ten_bit(&self) -> bool {
        self.max_bpc
            .as_ref()
            .is_none_or(|range| range.min <= 10 && range.max >= 10)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DisplayTransform {
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

render_elements! {
    pub DrmPresentElement<=GlesRenderer>;
    Texture=TextureRenderElement<GlesTexture>,
}

type OffscreenOutput = OffscreenTexture;

pub struct DrmRuntime {
    pub session: LibSeatSession,
    pub primary_gpu: DrmNode,
    pub devices: HashMap<DrmNode, DrmGpu>,
    pub should_stop: bool,
}

pub struct DrmGpu {
    pub node: DrmNode,
    pub fd: DrmDeviceFd,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub registration_token: RegistrationToken,

    pub render_node: Option<DrmNode>,

    pub allocator: GbmAllocator<DrmDeviceFd>,
    pub framebuffer_exporter: GbmFramebufferExporter<DrmDeviceFd>,
    pub drm_output_manager: DrmOutputManager<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,

    pub outputs: HashMap<crtc::Handle, DrmOutputState>,
}

pub struct DrmOutputState {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: Mode,
    pub output: Output,
    pub pending_modeset: bool,
}

/// Shared compositor core, without any synthetic nested-output assumptions.
pub(crate) struct CompositorCore {
    pub display: Display<DesktopState>,
    #[cfg(feature = "xwayland")]
    pub xwayland_event_loop: EventLoop<'static, DesktopState>,
    pub listener: ListeningSocket,
    pub wayland_display: String,
    pub state: DesktopState,
    pub clients: Vec<Client>,
    pub ui_state: UiState<GlesTexture>,
    pub scene: SceneState,
    pub output_state: OutputState,
    pub resources: RenderResources,
    pub start: Instant,
    pub last_now: Instant,
}

/// Per-CRTC/output state.
pub struct DrmSurfaceState {
    pub connector: connector::Handle,
    pub output: Output,
    pub mode: WlMode,
    pub size: Size<i32, Physical>,
    pub output_id: OutputId,
    pub origin: Point<i32, Logical>,
    pub present_render_id: Id,
    pub present_damage: DamageBag<i32, Buffer>,
    pub render_targets: LinearOffscreenTargets,
    pub hdr_support: HdrSupport,
    pub hdr_metadata_blob: Option<u64>,
    pub hdr_enabled_applied: bool,
    pub hdr_transition_target: Option<bool>,
    /// The pending HDR state was attached before this output's first commit,
    /// so the transition completion is the initial modeset rather than a live
    /// connector-property update.
    pub hdr_initial_modeset_pending: bool,
    pub hdr_render_supported: bool,
    pub frame_queued_at: Option<Instant>,
    /// Number of consecutive baseline vblanks observed since output setup.
    /// Live HDR is not staged until the SDR scanout path has proved healthy.
    pub stable_vblank_count: u8,
    /// Successful PQ vblanks completed after connector readback. User-visible
    /// HDR remains in "verifying" until this reaches `HDR_VERIFY_VBLANKS`.
    pub hdr_verify_vblank_count: u16,
    /// Start of the exclusive HDR verification window. Requiring elapsed time
    /// as well as frames prevents high-refresh displays from passing instantly.
    pub hdr_verify_started_at: Option<Instant>,
    /// Deadline for a pending HDR-transition commit to resolve via vblank.
    /// Only set when an HDR enable/disable transition was just queued; a
    /// stall past this deadline means the driver never delivered the
    /// completion event (see `stage_hdr_output_state` and the NONBLOCK
    /// comment on HDR commits in the vendored Smithay atomic backend).
    pub hdr_commit_deadline: Option<Instant>,
    pub hdr_dual_block_logged: bool,

    pub drm_output: DrmOutput<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        Option<OutputPresentationFeedback>,
        DrmDeviceFd,
    >,
}

const HDR_OFFSCREEN_FORMATS: [Fourcc; 2] = [Fourcc::Abgr2101010, Fourcc::Argb2101010];
const HDR_SCANOUT_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb2101010,
    Fourcc::Argb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Abgr2101010,
];
const DRM_SCANOUT_FORMAT_PREFERENCE: [Fourcc; 6] = [
    Fourcc::Xrgb2101010,
    Fourcc::Argb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Abgr2101010,
    Fourcc::Xrgb8888,
    Fourcc::Argb8888,
];
const HDR_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
const HDR_MIN_STABLE_VBLANKS: u8 = 3;
/// Keep submitting and completing HDR frames for roughly five seconds at the
/// minimum supported refresh before exposing the output as verified-active.
const HDR_VERIFY_VBLANKS: u16 = 300;
const HDR_VERIFY_DURATION: Duration = Duration::from_secs(5);
/// Keep SDR and HDR on the same conservative timing. High-refresh modes can fit
/// at 8 bpc but exceed the same connector's payload budget once the driver
/// switches to a 10-bpc link. Prefer the native-resolution mode at or below
/// this limit so an HDR transition does not also require a refresh-rate change.
const OUTPUT_MAX_REFRESH_HZ: u32 = 120;
/// Bound any queued DRM frame, not only HDR property transitions. A connector
/// disappearing between commit and vblank otherwise leaves the CRTC skipped forever.
const DRM_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const PCI_VENDOR_NVIDIA: u32 = 0x10de;

type FlowDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

fn queued_frame_stalled(queued_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(queued_at) >= DRM_FRAME_TIMEOUT
}

fn hdr_commit_stalled(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|deadline| now >= deadline)
}

fn hdr_active_status_verified(
    render_active: bool,
    exclusive_output: bool,
    verification_complete: bool,
) -> bool {
    render_active && (!exclusive_output || verification_complete)
}

fn hdr_verification_complete(
    verified_vblanks: u16,
    started_at: Option<Instant>,
    now: Instant,
) -> bool {
    verified_vblanks >= HDR_VERIFY_VBLANKS
        && started_at.is_some_and(|started_at| {
            now.saturating_duration_since(started_at) >= HDR_VERIFY_DURATION
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrmModeCandidate {
    width: u16,
    height: u16,
    refresh_hz: u32,
    preferred: bool,
}

fn select_drm_mode_index(candidates: &[DrmModeCandidate]) -> Option<usize> {
    let default_index = candidates
        .iter()
        .position(|candidate| candidate.preferred)
        .or((!candidates.is_empty()).then_some(0))?;

    let default = candidates[default_index];
    let within_refresh_limit =
        |candidate: &&DrmModeCandidate| (1..=OUTPUT_MAX_REFRESH_HZ).contains(&candidate.refresh_hz);

    // Preserve the preferred/native resolution and select its fastest timing
    // within the shared SDR/HDR limit. Nearly every display advertises 60 Hz,
    // with gaming displays commonly also advertising 100 or 120 Hz.
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.width == default.width
                && candidate.height == default.height
                && within_refresh_limit(candidate)
        })
        .max_by_key(|(_, candidate)| candidate.refresh_hz)
        .map(|(index, _)| index)
        // If the preferred resolution has no safe timing, retain the largest
        // advertised resolution that does rather than silently exceeding the
        // shared refresh ceiling.
        .or_else(|| {
            candidates
                .iter()
                .enumerate()
                .filter(|(_, candidate)| within_refresh_limit(candidate))
                .max_by_key(|(_, candidate)| {
                    (
                        u32::from(candidate.width) * u32::from(candidate.height),
                        candidate.refresh_hz,
                    )
                })
                .map(|(index, _)| index)
        })
        .or(Some(default_index))
}

fn select_connector_mode(modes: &[Mode]) -> Option<Mode> {
    let candidates: Vec<_> = modes
        .iter()
        .map(|mode| {
            let (width, height) = mode.size();
            DrmModeCandidate {
                width,
                height,
                refresh_hz: mode.vrefresh(),
                preferred: mode
                    .mode_type()
                    .contains(drm::control::ModeTypeFlags::PREFERRED),
            }
        })
        .collect();
    select_drm_mode_index(&candidates).map(|index| modes[index])
}

fn select_exclusive_hdr_target(
    selector: Option<&str>,
    candidates: &[(String, bool)],
) -> std::result::Result<Option<String>, String> {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some((name, capable)) = candidates.iter().find(|(name, _)| name == selector) else {
        return Err(format!("connector {selector:?} is not connected"));
    };
    if !capable {
        return Err(format!(
            "connector {selector:?} does not pass the preflight HDR safety checks"
        ));
    }
    Ok(Some(name.clone()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExclusiveHdrPrepareDecision {
    Start,
    Rearm,
    Skip,
    SkipUnclean,
}

fn exclusive_hdr_prepare_decision(
    phase: ExclusiveHdrPhase,
    same_process: bool,
) -> ExclusiveHdrPrepareDecision {
    match phase {
        ExclusiveHdrPhase::Failed | ExclusiveHdrPhase::Disabled | ExclusiveHdrPhase::Off => {
            ExclusiveHdrPrepareDecision::Skip
        }
        ExclusiveHdrPhase::Requested => ExclusiveHdrPrepareDecision::Start,
        ExclusiveHdrPhase::Starting | ExclusiveHdrPhase::Verifying | ExclusiveHdrPhase::Active
            if same_process =>
        {
            ExclusiveHdrPrepareDecision::Start
        }
        ExclusiveHdrPhase::Active => ExclusiveHdrPrepareDecision::Rearm,
        ExclusiveHdrPhase::Starting | ExclusiveHdrPhase::Verifying => {
            ExclusiveHdrPrepareDecision::SkipUnclean
        }
    }
}

fn persist_exclusive_hdr_phase(
    phase: ExclusiveHdrPhase,
    connector: Option<&str>,
    reason: Option<&str>,
) {
    let state = ExclusiveHdrState {
        phase,
        connector: connector.map(str::to_string),
        reason: reason.map(str::to_string),
        session_id: matches!(
            phase,
            ExclusiveHdrPhase::Starting
                | ExclusiveHdrPhase::Verifying
                | ExclusiveHdrPhase::Active
                | ExclusiveHdrPhase::Failed
        )
        .then_some(std::process::id()),
    };
    if let Err(err) = save_exclusive_hdr_state(&state) {
        flog_warn!("Failed to persist exclusive HDR state {phase:?}: {err}");
    }
}

fn prepare_exclusive_hdr_attempt(selector: Option<String>) -> Option<String> {
    let mut state = load_exclusive_hdr_state();
    let state_selector = state
        .phase
        .selects_output()
        .then(|| state.connector.clone())
        .flatten();
    let selector = selector.or(state_selector)?;
    let same_process = state.session_id == Some(std::process::id());

    match exclusive_hdr_prepare_decision(state.phase, same_process) {
        ExclusiveHdrPrepareDecision::Skip => {
            if state.phase == ExclusiveHdrPhase::Failed {
                flog_warn!(
                    "Exclusive HDR is fail-safe blocked on {}: {}",
                    state.connector.as_deref().unwrap_or(&selector),
                    state.reason.as_deref().unwrap_or("previous attempt failed")
                );
            }
            return None;
        }
        ExclusiveHdrPrepareDecision::SkipUnclean => {
            let reason = format!(
                "previous exclusive HDR session ended without a clean shutdown during {:?}",
                state.phase
            );
            state.phase = ExclusiveHdrPhase::Failed;
            state.reason = Some(reason.clone());
            state.session_id = Some(std::process::id());
            if let Err(err) = save_exclusive_hdr_state(&state) {
                flog_warn!("Failed to persist interrupted exclusive HDR attempt: {err}");
            }
            flog_warn!("Exclusive HDR fail-safe blocked: {reason}");
            return None;
        }
        ExclusiveHdrPrepareDecision::Rearm => {
            flog_warn!(
                "Previous exclusive HDR session on {selector} ended while Active; re-arming for this login"
            );
        }
        ExclusiveHdrPrepareDecision::Start => {}
    }

    persist_exclusive_hdr_phase(ExclusiveHdrPhase::Starting, Some(&selector), None);
    Some(selector)
}

/// Per-DRM-device backend state.
pub struct DrmDeviceState {
    pub registration_token: RegistrationToken,
    pub render_node: Option<DrmNode>,
    pub gpu_vendor_id: Option<u32>,
    pub renderer: GlesRenderer,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub drm_output_manager: FlowDrmOutputManager,
    pub surfaces: HashMap<drm::control::crtc::Handle, DrmSurfaceState>,
    /// Connector retained by the session-start-only exclusive HDR mode. When
    /// absent, every usable connected connector belongs to the active topology.
    pub exclusive_hdr_output: Option<String>,
}

/// Whole backend state for tty/udev/libinput/drm.
pub struct DrmBackend {
    pub session: LibSeatSession,
    pub primary_gpu: DrmNode,
    pub devices: HashMap<DrmNode, DrmDeviceState>,
}

/// Loop data for the DRM backend.
pub(crate) struct DrmLoopData {
    pub core: CompositorCore,
    pub backend: DrmBackend,
    pub libinput: Libinput,
    pub session_active: bool,
    /// A resume can be announced by login1 before libseat has returned DRM
    /// ownership. Keep the session paused until the existing DRM output
    /// managers can be activated again.
    pub resume_pending: bool,
    pub resume_retry_at: Option<Instant>,
    /// Device rebuilds requested from inside a DRM event callback after an
    /// exclusive HDR validation failure. Rebuilding with failed persistent
    /// state restores the ordinary all-output SDR topology.
    pub exclusive_hdr_recovery_nodes: Vec<DrmNode>,
    pub should_stop: bool,
}

fn display_config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });

    base.join("focaldesk").join("displays.json")
}

fn load_display_config() -> Vec<DisplayConfig> {
    let path = display_config_path();

    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(displays) => displays,
            Err(err) => {
                flog(&format!(
                    "Failed to parse display config {}; using defaults: {}",
                    path.display(),
                    err
                ));
                Vec::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            flog(&format!(
                "Failed to read display config {}; using defaults: {}",
                path.display(),
                err
            ));
            Vec::new()
        }
    }
}

fn configured_display_scale(displays: &[DisplayConfig], name: &str) -> f64 {
    const MIN_SCALE: f64 = 1.0;
    const MAX_SCALE: f64 = 4.0;

    let Some(scale) = displays
        .iter()
        .find(|display| display.name == name)
        .map(|display| display.scale)
    else {
        return MIN_SCALE;
    };

    if !scale.is_finite() || scale < MIN_SCALE {
        flog(&format!(
            "Ignoring invalid display scale for {}: {}; using {}",
            name, scale, MIN_SCALE
        ));
        return MIN_SCALE;
    }

    if scale > MAX_SCALE {
        flog(&format!(
            "Clamping display scale for {} from {} to {}",
            name, scale, MAX_SCALE
        ));
        return MAX_SCALE;
    }

    scale
}

fn configured_display_hdr_requested(displays: &[DisplayConfig], name: &str) -> bool {
    displays
        .iter()
        .find(|display| display.name == name)
        .map(|display| display.hdr_requested || display.hdr_enabled)
        .unwrap_or(false)
}

/// Remember that exclusive HDR verified on this connector so the next ordinary
/// session can Apply Requested HDR10 even if exclusive mode is not re-armed.
fn enable_persisted_hdr_request(output_name: &str) {
    let mut displays = load_display_config();
    let mut changed = false;
    for display in displays.iter_mut() {
        if display.name == output_name && !display.hdr_requested {
            display.hdr_requested = true;
            changed = true;
        }
    }
    if changed {
        if let Err(err) = write_display_config(&displays) {
            flog_warn!("Failed to persist HDR request for {output_name}: {err}");
        }
    }
}

/// Persist that `name` should not auto-request HDR again (used after a stalled HDR commit
/// forces recovery). The user can still re-enable HDR explicitly through settings.
fn disable_persisted_hdr_request(output_name: &str) {
    let mut displays = load_display_config();
    let mut changed = false;
    for display in displays.iter_mut() {
        if display.name == output_name && (display.hdr_requested || display.hdr_enabled) {
            display.hdr_requested = false;
            display.hdr_enabled = false;
            changed = true;
        }
    }
    if changed {
        if let Err(err) = write_display_config(&displays) {
            flog_warn!("Failed to persist HDR auto-disable for {output_name}: {err}");
        }
    }
}

fn disable_all_persisted_hdr_requests() {
    let mut displays = load_display_config();
    let mut changed = false;
    for display in displays.iter_mut() {
        if display.hdr_requested || display.hdr_enabled {
            display.hdr_requested = false;
            display.hdr_enabled = false;
            changed = true;
        }
    }
    if changed {
        if let Err(err) = write_display_config(&displays) {
            flog_warn!("Failed to persist HDR auto-disable for all outputs: {err}");
        }
    }
}

/// Exclusive HDR failures latch `Failed` and restore the ordinary topology.
/// Persist-disabling only the exclusive connector leaves sibling outputs in
/// HDR10 while this one falls back to SDR+ICC, so identical panels no longer
/// match. Ordinary NVIDIA dual-head failures disable every request instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HdrFailurePersist {
    KeepRequest,
    DisableOne,
    DisableAll,
}

fn hdr_failure_persist_action(
    exclusive_output: Option<&str>,
    failed_output: &str,
    nvidia_dual: bool,
) -> HdrFailurePersist {
    if exclusive_output == Some(failed_output) {
        HdrFailurePersist::KeepRequest
    } else if nvidia_dual {
        HdrFailurePersist::DisableAll
    } else {
        HdrFailurePersist::DisableOne
    }
}

fn apply_persisted_hdr_failure(action: HdrFailurePersist, failed_output: &str) {
    match action {
        HdrFailurePersist::KeepRequest => {}
        HdrFailurePersist::DisableOne => disable_persisted_hdr_request(failed_output),
        HdrFailurePersist::DisableAll => disable_all_persisted_hdr_requests(),
    }
}

fn clear_runtime_hdr_request(output: &mut crate::core::desktop::OutputState) {
    output.hdr_requested = false;
    output.hdr_verification_pending = false;
    output.hdr_enabled = false;
}

fn configured_display_color_profile(displays: &[DisplayConfig], name: &str) -> DisplayColorProfile {
    displays
        .iter()
        .find(|display| display.name == name)
        .map(|display| display.color_profile)
        .unwrap_or_default()
}

fn write_display_config(displays: &[DisplayConfig]) -> Result<()> {
    let path = display_config_path();
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("display config path has no parent: {}", path.display()))?;

    std::fs::create_dir_all(&dir)?;

    let json = serde_json::to_string_pretty(displays)?;
    std::fs::write(&path, json)?;

    flog(&format!("Wrote display config to {}", path.display()));

    Ok(())
}

/// Convert bottom-up GL BGRA bytes to top-down RGBA.
fn bgra_gl_bottom_left_to_rgba(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    debug_assert_eq!(src.len(), width * height * 4);
    let stride = width * 4;
    let mut out = vec![0u8; src.len()];
    for y in 0..height {
        let src_row = y * stride;
        let dst_row = (height - 1 - y) * stride;
        for x in 0..width {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            out[d] = src[s + 2];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s];
            out[d + 3] = src[s + 3];
        }
    }
    out
}

/// Read pixels from the bound GLES framebuffer (same FBO the frame was rendered to).
///
/// DRM offscreen uses [`Fourcc::Abgr8888`] (`GL_RGBA8` + `GL_RGBA` read), which tends to be
/// reliable on GLES. `Fourcc::Argb8888` uses `GL_BGRA_EXT` readback; some stacks return zeros
/// from FBO read despite a valid draw.
fn copy_framebuffer_target_to_rgba8(
    renderer: &mut GlesRenderer,
    target: &GlesTarget<'_>,
    width: i32,
    height: i32,
) -> Result<Vec<u8>> {
    use smithay::backend::renderer::gles::ffi;

    let w = width as usize;
    let h = height as usize;

    renderer
        .with_context(|gl| unsafe {
            gl.BindBuffer(ffi::PIXEL_PACK_BUFFER, 0);
        })
        .map_err(|e| anyhow!("screenshot GL state: {e}"))?;

    let region = Rectangle::<i32, Buffer>::from_loc_and_size(
        Point::from((0, 0)),
        Size::from((width, height)),
    );

    match renderer.copy_framebuffer(target, region, Fourcc::Abgr8888) {
        Ok(mapping) => {
            renderer
                .with_context(|gl| unsafe {
                    gl.Finish();
                })
                .map_err(|e| anyhow!("screenshot Finish: {e}"))?;
            let src = renderer
                .map_texture(&mapping)
                .map_err(|e| anyhow!("map_texture (ABGR8888): {e}"))?;
            Ok(src.to_vec())
        }
        Err(e1) => {
            flog(&format!(
                "screenshot: ABGR8888 read failed ({e1}), trying ARGB8888/BGRA"
            ));
            let mapping = renderer
                .copy_framebuffer(target, region, Fourcc::Argb8888)
                .map_err(|e2| anyhow!("copy_framebuffer ARGB8888 after ABGR fail: {e2}"))?;
            renderer
                .with_context(|gl| unsafe {
                    gl.Finish();
                })
                .map_err(|e| anyhow!("screenshot Finish: {e}"))?;
            let src = renderer
                .map_texture(&mapping)
                .map_err(|e| anyhow!("map_texture (ARGB8888): {e}"))?;
            Ok(bgra_gl_bottom_left_to_rgba(src, w, h))
        }
    }
}

fn copy_linear_scene_rgba16f(
    renderer: &mut GlesRenderer,
    texture: &mut GlesTexture,
    width: i32,
    height: i32,
) -> Result<Vec<u8>> {
    use smithay::backend::renderer::gles::ffi;

    let mut pixels = vec![0u16; width as usize * height as usize * 4];
    let texture_id = texture.tex_id();
    // Binding waits for outstanding writes to the scene texture.  Attach it to
    // a private FBO because GLES has no desktop GL `GetTexImage` equivalent.
    let _target = renderer
        .bind(texture)
        .map_err(|e| anyhow!("bind FP16 screenshot target: {e}"))?;
    renderer
        .with_context(|gl| unsafe {
            let mut framebuffer = 0;
            gl.GenFramebuffers(1, &mut framebuffer);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffer);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                texture_id,
                0,
            );
            let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
            gl.BindBuffer(ffi::PIXEL_PACK_BUFFER, 0);
            gl.PixelStorei(ffi::PACK_ALIGNMENT, 1);
            if status == ffi::FRAMEBUFFER_COMPLETE {
                gl.ReadPixels(
                    0,
                    0,
                    width,
                    height,
                    ffi::RGBA,
                    ffi::HALF_FLOAT,
                    pixels.as_mut_ptr().cast(),
                );
            }
            gl.Finish();
            let error = gl.GetError();
            gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
            gl.DeleteFramebuffers(1, &framebuffer);
            (status, error)
        })
        .map_err(|e| anyhow!("screenshot FP16 GL state: {e}"))
        .and_then(|(status, error)| {
            anyhow::ensure!(
                status == ffi::FRAMEBUFFER_COMPLETE,
                "screenshot FP16 framebuffer incomplete: 0x{status:04x}"
            );
            anyhow::ensure!(
                error == ffi::NO_ERROR,
                "screenshot FP16 read failed: GL error 0x{error:04x}"
            );
            Ok(())
        })?;

    let len = pixels.len() * 2;
    let bytes = unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), len) };
    Ok(bytes.to_vec())
}

fn capture_surface_pixels(
    renderer: &mut GlesRenderer,
    surface: &mut DrmSurfaceState,
) -> Result<Vec<u16>> {
    if surface.render_targets.scene_linear {
        let scene = surface
            .render_targets
            .linear_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("linear scene missing for capture"))?;
        let raw =
            copy_linear_scene_rgba16f(renderer, &mut scene.texture, scene.size.w, scene.size.h)?;
        crate::core::screenshot::linear_scene_f16_to_display_p3_rgb16(
            &raw,
            scene.size.w as usize,
            scene.size.h as usize,
        )
    } else {
        let offscreen = surface
            .render_targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("offscreen texture missing for capture"))?;
        let target = renderer
            .bind(&mut offscreen.texture)
            .map_err(|e| anyhow!("bind sRGB scene for capture: {e}"))?;
        let raw = copy_framebuffer_target_to_rgba8(
            renderer,
            &target,
            offscreen.size.w,
            offscreen.size.h,
        )?;
        crate::core::screenshot::srgb_rgba8_to_display_p3_rgb16(
            &raw,
            offscreen.size.w as usize,
            offscreen.size.h as usize,
        )
    }
}

fn present_source_texture(surface: &DrmSurfaceState) -> Option<&GlesTexture> {
    surface.render_targets.scanout_texture()
}

fn capture_source_texture(
    surface: &DrmSurfaceState,
) -> Option<(GlesTexture, crate::core::portal::PortalCaptureEncoding)> {
    crate::core::portal::portal_source_from_targets(&surface.render_targets)
}

fn blit_rgb16(
    dst: &mut [u16],
    dst_width: usize,
    dst_height: usize,
    src: &[u16],
    src_width: usize,
    src_height: usize,
    dst_x: usize,
    dst_y: usize,
) -> Result<()> {
    if dst_x + src_width > dst_width || dst_y + src_height > dst_height {
        return Err(anyhow!("blit out of bounds"));
    }

    let dst_stride = dst_width * 3;
    let src_stride = src_width * 3;

    for row in 0..src_height {
        let src_start = row * src_stride;
        let src_end = src_start + src_stride;

        let dst_start = (dst_y + row) * dst_stride + dst_x * 3;
        let dst_end = dst_start + src_stride;

        dst[dst_start..dst_end].copy_from_slice(&src[src_start..src_end]);
    }

    Ok(())
}

fn save_all_outputs_screenshot(
    renderer: &mut GlesRenderer,
    surfaces: &mut HashMap<drm::control::crtc::Handle, DrmSurfaceState>,
    seq: u64,
) -> Result<PathBuf> {
    if surfaces.is_empty() {
        return Err(anyhow!("no DRM surfaces available for screenshot"));
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for surface in surfaces.values() {
        min_x = min_x.min(surface.origin.x);
        min_y = min_y.min(surface.origin.y);
        max_x = max_x.max(surface.origin.x + surface.size.w);
        max_y = max_y.max(surface.origin.y + surface.size.h);
    }

    let total_width = (max_x - min_x) as usize;
    let total_height = (max_y - min_y) as usize;

    let mut desktop_pixels = vec![0u16; total_width * total_height * 3];

    for surface in surfaces.values_mut() {
        let pixels = capture_surface_pixels(renderer, surface)?;
        let width = surface.size.w as usize;
        let height = surface.size.h as usize;

        let dst_x = (surface.origin.x - min_x) as usize;
        let dst_y = (surface.origin.y - min_y) as usize;

        blit_rgb16(
            &mut desktop_pixels,
            total_width,
            total_height,
            &pixels,
            width,
            height,
            dst_x,
            dst_y,
        )?;
    }

    save_screenshot_png(
        total_width as i32,
        total_height as i32,
        desktop_pixels,
        "all-outputs",
        seq,
    )
}

fn save_screenshot_png(
    width: i32,
    height: i32,
    pixels: Vec<u16>,
    output_name: &str,
    seq: u64,
) -> Result<PathBuf> {
    use chrono::Local;
    use std::fs;
    use std::path::PathBuf;

    let screenshot_dir =
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
            .join("Pictures")
            .join("Screenshots");

    fs::create_dir_all(&screenshot_dir)?;

    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let filename = format!("focaldesk-{}-{}-{}.png", output_name, timestamp, seq);
    let path = screenshot_dir.join(filename);

    crate::core::screenshot::write_display_p3_png(&path, width as u32, height as u32, &pixels)?;
    flog(&format!("Screenshot saved to {}", path.display()));
    Ok(path)
}

fn save_offscreen_screenshot(
    renderer: &mut GlesRenderer,
    surface: &mut DrmSurfaceState,
    output_name: &str,
    seq: u64,
) -> Result<PathBuf> {
    let size = surface.size;

    let pixels = capture_surface_pixels(renderer, surface)?;

    let screenshot_dir =
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
            .join("Pictures")
            .join("Screenshots");

    fs::create_dir_all(&screenshot_dir)?;

    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let filename = format!("focaldesk-{}-{}-{}.png", output_name, timestamp, seq);
    let path = screenshot_dir.join(filename);

    crate::core::screenshot::write_display_p3_png(&path, size.w as u32, size.h as u32, &pixels)?;
    Ok(path)
}

fn remove_drm_device(
    data: &mut DrmLoopData,
    loop_handle: &LoopHandle<'_, DrmLoopData>,
    node: DrmNode,
) {
    if let Some(device) = data.backend.devices.remove(&node) {
        let _ = loop_handle.remove(device.registration_token);

        for surface in device.surfaces.into_values() {
            data.core.state.space.unmap_output(&surface.output);
            data.core.state.outputs.shift_remove(&surface.output_id);
            data.core
                .state
                .desktop_outputs
                .shift_remove(&surface.output_id);
            data.core
                .output_state
                .outputs
                .shift_remove(&surface.output_id);
        }

        if let Some(output_id) = data.core.state.outputs.keys().next().copied() {
            data.core.state.primary_output = output_id;
            data.core.state.focused_output = output_id;
        }

        data.core.state.mark_redraw();
    }
}

fn drm_connector_topology_changed(device: &DrmDeviceState, state: &DesktopState) -> Result<bool> {
    let resources = device
        .drm_output_manager
        .device()
        .resource_handles()
        .context("failed to query DRM resources after hotplug")?;
    let mut connected = std::collections::HashSet::new();

    for connector in resources.connectors() {
        let info = device
            .drm_output_manager
            .device()
            .get_connector(*connector, false)
            .context("failed to query DRM connector after hotplug")?;
        if info.state() != drm::control::connector::State::Connected {
            continue;
        }
        let output_name = connector_name(&info);
        if device
            .exclusive_hdr_output
            .as_deref()
            .is_some_and(|selected| selected != output_name)
        {
            continue;
        }
        connected.insert(*connector);

        let Some(surface) = device
            .surfaces
            .values()
            .find(|surface| surface.connector == *connector)
        else {
            return Ok(true);
        };

        let selected_mode = select_connector_mode(info.modes());
        if selected_mode.as_ref().is_none_or(|mode| {
            let (width, height) = mode.size();
            surface.mode.size != Size::from((i32::from(width), i32::from(height)))
                || surface.mode.refresh != (mode.vrefresh() as i32).max(60) * 1000
        }) {
            return Ok(true);
        }

        let current_edid = connector_edid(device.drm_output_manager.device(), *connector);
        let advertised_edid = state
            .outputs
            .get(&surface.output_id)
            .and_then(|output| output.monitor_edid.as_ref());
        if current_edid.as_ref() != advertised_edid {
            return Ok(true);
        }
    }

    let active: std::collections::HashSet<_> = device
        .surfaces
        .values()
        .map(|surface| surface.connector)
        .collect();
    Ok(connected != active)
}

fn reinitialize_drm_device(
    data: &mut DrmLoopData,
    loop_handle: &LoopHandle<'_, DrmLoopData>,
    node: DrmNode,
) -> Result<()> {
    let path = node
        .dev_path()
        .ok_or_else(|| anyhow!("failed to resolve DRM path for {:?}", node))?;

    flog(&format!(
        "Reinitializing DRM device {:?} via {}",
        node,
        path.display()
    ));

    let topology = data.core.state.snapshot_output_topology();
    remove_drm_device(data, loop_handle, node);
    device_added(data, loop_handle, node, &path)?;
    data.core.state.restore_output_topology(topology);
    refresh_portal_services(&data.core.state.client_wayland_display);
    data.core
        .state
        .mark_all_outputs_full_damage(DamageSource::Unknown);
    data.core.state.mark_redraw();

    Ok(())
}

fn pause_drm_session(data: &mut DrmLoopData, reason: &str) {
    data.resume_pending = true;
    data.resume_retry_at = None;

    if !data.session_active {
        return;
    }

    // Keep this at warning level so production journals retain the exact
    // suspend/resume ordering.  Resume failures are impossible to diagnose if
    // the last visible compositor event predates PrepareForSleep(true).
    flog_warn!("Pausing DRM session ({reason})");
    data.core.state.handle_session_suspend();
    data.session_active = false;
    data.libinput.suspend();
    for device in data.backend.devices.values_mut() {
        device.drm_output_manager.pause();
    }
}

fn resume_drm_session(data: &mut DrmLoopData, reason: &str) {
    // PrepareForSleep(false) is delivered on a separate D-Bus connection and
    // can win the race with libseat's ActivateSession. Defer until libseat has
    // restored ownership; activating a paused DRM manager before that point
    // cannot succeed.
    if !data.backend.session.is_active() {
        flog_warn!("Deferring DRM resume until libseat returns device ownership ({reason})");
        data.resume_pending = true;
        data.resume_retry_at = Some(Instant::now() + Duration::from_millis(250));
        return;
    }

    if data.session_active && !data.resume_pending {
        flog(&format!(
            "Ignoring duplicate DRM resume notification ({reason})"
        ));
        return;
    }

    flog_warn!("Resuming DRM session ({reason})");
    data.session_active = false;
    if let Err(err) = data.libinput.resume() {
        flog(&format!("Failed to resume libinput: {err:?}"));
    }

    // libseat keeps ownership of every device opened through the session and
    // restores those file descriptors before ActivateSession is emitted. Do
    // not close and reopen them here: requesting the same device from libseat
    // a second time fails with EINVAL on seatd/libseat, and removing the old
    // device first also destroys the only output state that can be resumed.
    // Smithay's intended suspend/resume pair is DrmOutputManager::pause() and
    // DrmOutputManager::activate().
    let mut all_devices_ready = true;
    if data.backend.devices.is_empty() {
        flog_warn!("No DRM devices are available after resume; scheduling retry");
        all_devices_ready = false;
    }
    for (node, device) in &mut data.backend.devices {
        if let Err(err) = device.drm_output_manager.lock().activate(false) {
            flog_warn!(
                "Failed to reactivate DRM device {:?} after resume: {err}; scheduling retry",
                node
            );
            all_devices_ready = false;
        }
    }

    if !all_devices_ready {
        data.resume_retry_at = Some(Instant::now() + Duration::from_millis(250));
        return;
    }

    data.resume_pending = false;
    data.resume_retry_at = None;
    data.core.state.handle_session_resume();
    // RenderState::invalidate_gpu_state() (called above via
    // handle_session_resume) only covers caches owned by DesktopState.
    data.core.ui_state.chrome.invalidate_gpu_state();
    data.core.last_now = Instant::now();
    data.core.state.mark_redraw();
    data.session_active = true;
}

/// Live KMS HDR application is compiled in but remains runtime opt-in. NVIDIA also
/// requires explicit driver and topology overrides because earlier commits froze.
const HDR_LIVE_KMS_APPLY_ENABLED: bool = true;

/// Live scanout/connector HDR changes honor the settings toggle. Set `FOCALDESK_HDR=0` to block.
fn hdr_runtime_apply_enabled(any_output_hdr_requested: bool) -> bool {
    HDR_LIVE_KMS_APPLY_ENABLED
        && crate::core::color::hdr_runtime_may_apply_kms(any_output_hdr_requested)
}

fn hdr_driver_allows_output(gpu_vendor_id: Option<u32>) -> bool {
    let allow_nvidia = matches!(
        std::env::var("FOCALDESK_HDR_ALLOW_NVIDIA").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    );

    hdr_driver_allows_output_with_override(gpu_vendor_id, allow_nvidia)
}

fn hdr_driver_allows_output_with_override(gpu_vendor_id: Option<u32>, allow_nvidia: bool) -> bool {
    gpu_vendor_id != Some(PCI_VENDOR_NVIDIA) || allow_nvidia
}

fn nvidia_dual_head_hdr_allowed() -> bool {
    crate::core::color::hdr_nvidia_dual_enabled()
}

/// NVIDIA HDR requires the driver override in every topology. Non-exclusive
/// topologies additionally require an explicit dual-head override so ordinary
/// sessions do not inherit the risk of live multi-output KMS changes.
fn nvidia_kms_hdr_blocked(gpu_vendor_id: Option<u32>, exclusive_output: bool) -> bool {
    let driver_allowed = hdr_driver_allows_output(gpu_vendor_id);
    let dual_head_allowed = nvidia_dual_head_hdr_allowed();
    nvidia_kms_hdr_blocked_with_override(
        gpu_vendor_id,
        exclusive_output,
        driver_allowed,
        dual_head_allowed,
    )
}

fn nvidia_kms_hdr_blocked_with_override(
    gpu_vendor_id: Option<u32>,
    exclusive_output: bool,
    driver_allowed: bool,
    dual_head_allowed: bool,
) -> bool {
    gpu_vendor_id == Some(PCI_VENDOR_NVIDIA)
        && (!driver_allowed || (!exclusive_output && !dual_head_allowed))
}

/// Sync `OutputState` HDR flags from EDID/KMS detection and persisted config.
fn sync_output_hdr_flags(
    output: &mut crate::core::desktop::OutputState,
    support: &HdrSupport,
    hdr_requested_from_config: bool,
) {
    // Report the display's EDID capability independently from the KMS safety
    // policy. In particular, NVIDIA outputs must remain visible as HDR-capable
    // in Display Settings even when live HDR commits require an explicit
    // override. `hdr_output_capable` and the commit path enforce that policy.
    output.hdr_supported = support.is_detected();
    output.hdr_requested = hdr_requested_from_config && output.hdr_supported;
    output.hdr_verification_pending = false;
    if let Some(meta) = support.edid_hdr_metadata.as_ref() {
        output.edid_hdr_max_luminance_nits = Some(meta.max_luminance as f32);
        output.edid_hdr_max_fall_nits = Some(meta.max_fall as f32);
    } else {
        output.edid_hdr_max_luminance_nits = None;
        output.edid_hdr_max_fall_nits = None;
    }
    output.hdr_enabled = crate::core::color::output_hdr_render_active(
        output.hdr_requested,
        output.hdr_supported,
        output.hdr_kms_applied,
    );
}

#[cfg(test)]
mod screenshot_tests {
    use super::bgra_gl_bottom_left_to_rgba;

    #[test]
    fn bgra_fallback_converts_channels_and_gl_row_order() {
        let bottom_up_bgra = [
            255, 0, 0, 255, // bottom-left: blue
            255, 255, 255, 255, // bottom-right: white
            0, 0, 255, 255, // top-left: red
            0, 255, 0, 255, // top-right: green
        ];

        assert_eq!(
            bgra_gl_bottom_left_to_rgba(&bottom_up_bgra, 2, 2),
            [
                255, 0, 0, 255, // top-left: red
                0, 255, 0, 255, // top-right: green
                0, 0, 255, 255, // bottom-left: blue
                255, 255, 255, 255, // bottom-right: white
            ]
        );
    }
}

#[cfg(test)]
mod hdr_tests {
    use super::{
        configured_display_hdr_requested, exclusive_hdr_prepare_decision,
        hdr_active_status_verified, hdr_commit_stalled, hdr_detection::parse_edid_hdr_support,
        hdr_driver_allows_output_with_override, hdr_failure_persist_action,
        hdr_verification_complete, merge_disconnected_display_configs,
        nvidia_kms_hdr_blocked_with_override, queued_frame_stalled, select_drm_mode_index,
        select_exclusive_hdr_target, DisplayConfig, DisplayTransform, DrmModeCandidate,
        EdidHdrMetadata, ExclusiveHdrPrepareDecision, HdrBpcRange, HdrFailurePersist, HdrSupport,
        DRM_FRAME_TIMEOUT, DRM_SCANOUT_FORMAT_PREFERENCE, HDR_FRAME_TIMEOUT, HDR_SCANOUT_FORMATS,
        HDR_VERIFY_DURATION, HDR_VERIFY_VBLANKS, OUTPUT_MAX_REFRESH_HZ, PCI_VENDOR_NVIDIA,
    };
    use focaldesk_settings_core::{DisplayColorProfile, ExclusiveHdrPhase};
    use std::time::{Duration, Instant};

    fn display_config(hdr_requested: bool, hdr_enabled: bool) -> DisplayConfig {
        DisplayConfig {
            name: "DP-1".into(),
            enabled: true,
            mode_width: 2560,
            mode_height: 1440,
            refresh_mhz: 165_000,
            scale: 1.0,
            logical_x: 0,
            logical_y: 0,
            physical_width_mm: None,
            physical_height_mm: None,
            primary: true,
            transform: DisplayTransform::Normal,
            hdr_supported: true,
            hdr_requested,
            hdr_enabled,
            color_profile: DisplayColorProfile::Auto,
            icc_profile_path: None,
        }
    }

    #[test]
    fn loads_persistent_hdr_request_independently_of_runtime_state() {
        assert!(configured_display_hdr_requested(
            &[display_config(true, false)],
            "DP-1"
        ));
        // Preserve compatibility with configs written before hdr_requested existed.
        assert!(configured_display_hdr_requested(
            &[display_config(false, true)],
            "DP-1"
        ));
    }

    #[test]
    fn exclusive_hdr_failure_keeps_ordinary_hdr_request() {
        assert_eq!(
            hdr_failure_persist_action(Some("DP-3"), "DP-3", true),
            HdrFailurePersist::KeepRequest
        );
        assert_eq!(
            hdr_failure_persist_action(None, "DP-3", true),
            HdrFailurePersist::DisableAll
        );
        assert_eq!(
            hdr_failure_persist_action(None, "DP-3", false),
            HdrFailurePersist::DisableOne
        );
    }

    #[test]
    fn exclusive_hdr_rearms_active_state_after_restart_or_shutdown() {
        assert_eq!(
            exclusive_hdr_prepare_decision(ExclusiveHdrPhase::Active, false),
            ExclusiveHdrPrepareDecision::Rearm
        );
        assert_eq!(
            exclusive_hdr_prepare_decision(ExclusiveHdrPhase::Requested, false),
            ExclusiveHdrPrepareDecision::Start
        );
        assert_eq!(
            exclusive_hdr_prepare_decision(ExclusiveHdrPhase::Failed, false),
            ExclusiveHdrPrepareDecision::Skip
        );
        assert_eq!(
            exclusive_hdr_prepare_decision(ExclusiveHdrPhase::Verifying, false),
            ExclusiveHdrPrepareDecision::SkipUnclean
        );
        assert_eq!(
            exclusive_hdr_prepare_decision(ExclusiveHdrPhase::Active, true),
            ExclusiveHdrPrepareDecision::Start
        );
    }

    #[test]
    fn exclusive_hdr_target_requires_an_exact_capable_connector() {
        let candidates = vec![("DP-3".to_string(), true), ("HDMI-A-1".to_string(), false)];
        assert_eq!(
            select_exclusive_hdr_target(Some(" DP-3 "), &candidates).unwrap(),
            Some("DP-3".to_string())
        );
        assert!(select_exclusive_hdr_target(Some("DP-4"), &candidates).is_err());
        assert!(select_exclusive_hdr_target(Some("HDMI-A-1"), &candidates).is_err());
        assert_eq!(
            select_exclusive_hdr_target(None, &candidates).unwrap(),
            None
        );
    }

    #[test]
    fn lost_vblank_is_bounded_even_outside_an_hdr_transition() {
        let queued_at = Instant::now();
        assert!(!queued_frame_stalled(
            queued_at,
            queued_at + DRM_FRAME_TIMEOUT - std::time::Duration::from_millis(1)
        ));
        assert!(queued_frame_stalled(
            queued_at,
            queued_at + DRM_FRAME_TIMEOUT
        ));
    }

    #[test]
    fn hdr_transition_timeout_does_not_require_a_queued_frame() {
        let now = Instant::now();
        assert!(!hdr_commit_stalled(None, now));
        assert!(!hdr_commit_stalled(Some(now + HDR_FRAME_TIMEOUT), now));
        assert!(hdr_commit_stalled(Some(now), now));
    }

    #[test]
    fn exclusive_hdr_is_not_reported_active_before_stability_window() {
        let started_at = Instant::now();
        assert!(!hdr_verification_complete(
            HDR_VERIFY_VBLANKS - 1,
            Some(started_at),
            started_at + HDR_VERIFY_DURATION
        ));
        assert!(!hdr_verification_complete(
            HDR_VERIFY_VBLANKS,
            Some(started_at),
            started_at + HDR_VERIFY_DURATION - Duration::from_millis(1)
        ));
        assert!(hdr_verification_complete(
            HDR_VERIFY_VBLANKS,
            Some(started_at),
            started_at + HDR_VERIFY_DURATION
        ));
        assert!(!hdr_active_status_verified(true, true, false));
        assert!(hdr_active_status_verified(true, true, true));
        assert!(hdr_active_status_verified(true, false, false));
        assert!(!hdr_active_status_verified(false, true, true));
    }

    #[test]
    fn scanout_preferences_try_ten_bit_before_sdr_fallbacks() {
        assert!(DRM_SCANOUT_FORMAT_PREFERENCE[..4]
            .iter()
            .all(|format| HDR_SCANOUT_FORMATS.contains(format)));
        assert!(DRM_SCANOUT_FORMAT_PREFERENCE[4..]
            .iter()
            .all(|format| !HDR_SCANOUT_FORMATS.contains(format)));
    }

    #[test]
    fn output_mode_selection_caps_native_resolution_at_120_hz() {
        let candidates = [
            DrmModeCandidate {
                width: 2560,
                height: 1440,
                refresh_hz: 165,
                preferred: true,
            },
            DrmModeCandidate {
                width: 2560,
                height: 1440,
                refresh_hz: OUTPUT_MAX_REFRESH_HZ,
                preferred: false,
            },
            DrmModeCandidate {
                width: 2560,
                height: 1440,
                refresh_hz: 60,
                preferred: false,
            },
        ];

        assert_eq!(select_drm_mode_index(&candidates), Some(1));
    }

    #[test]
    fn output_mode_selection_never_chooses_faster_mode_when_safe_mode_exists() {
        let candidates = [
            DrmModeCandidate {
                width: 3840,
                height: 2160,
                refresh_hz: 144,
                preferred: true,
            },
            DrmModeCandidate {
                width: 2560,
                height: 1440,
                refresh_hz: 120,
                preferred: false,
            },
            DrmModeCandidate {
                width: 3840,
                height: 2160,
                refresh_hz: 60,
                preferred: false,
            },
        ];

        assert_eq!(select_drm_mode_index(&candidates), Some(2));
    }

    #[test]
    fn nvidia_hdr_requires_explicit_driver_and_topology_overrides() {
        assert!(nvidia_kms_hdr_blocked_with_override(
            Some(PCI_VENDOR_NVIDIA),
            false,
            true,
            false,
        ));
        assert!(nvidia_kms_hdr_blocked_with_override(
            Some(PCI_VENDOR_NVIDIA),
            true,
            false,
            true,
        ));
        assert!(!nvidia_kms_hdr_blocked_with_override(
            Some(PCI_VENDOR_NVIDIA),
            true,
            true,
            false,
        ));
        assert!(!nvidia_kms_hdr_blocked_with_override(
            Some(PCI_VENDOR_NVIDIA),
            false,
            true,
            true,
        ));
        assert!(!nvidia_kms_hdr_blocked_with_override(
            Some(0x1002),
            false,
            false,
            false,
        ));
    }

    #[test]
    fn hdr_capability_accepts_driver_managed_bpc_with_ten_bit_scanout() {
        let mut support = HdrSupport {
            has_hdr_metadata_property: true,
            has_bt2020_colorspace: true,
            edid_hdr_static_metadata: true,
            edid_static_metadata_type1: true,
            edid_pq: true,
            edid_hdr_metadata: Some(EdidHdrMetadata {
                display_primaries: [(1, 1); 3],
                white_point: (1, 1),
                max_luminance: 1_000,
                min_luminance: 1,
                max_fall: 400,
            }),
            ..HdrSupport::default()
        };
        assert!(!support.can_enable(false));
        assert!(support.can_enable(true));
        support.max_bpc = Some(HdrBpcRange { min: 8, max: 8 });
        assert!(!support.can_enable(true));
        support.max_bpc = Some(HdrBpcRange { min: 8, max: 12 });
        assert!(support.can_enable(true));
    }

    #[test]
    fn disconnected_display_preferences_survive_topology_rebuild() {
        let connected = display_config(false, false);
        let mut disconnected = display_config(true, true);
        disconnected.name = "HDMI-A-1".into();
        disconnected.logical_x = 2560;

        let merged = merge_disconnected_display_configs(vec![connected], &[disconnected]);
        let saved = merged
            .iter()
            .find(|display| display.name == "HDMI-A-1")
            .expect("disconnected display should remain persisted");
        assert!(!saved.enabled);
        assert!(saved.hdr_requested);
        assert!(!saved.hdr_enabled);
        assert_eq!(saved.logical_x, 2560);
    }

    #[test]
    fn nvidia_hdr_requires_explicit_override() {
        assert!(!hdr_driver_allows_output_with_override(
            Some(PCI_VENDOR_NVIDIA),
            false
        ));
        assert!(hdr_driver_allows_output_with_override(
            Some(PCI_VENDOR_NVIDIA),
            true
        ));
    }

    #[test]
    fn other_and_unknown_drivers_remain_allowed() {
        assert!(hdr_driver_allows_output_with_override(Some(0x8086), false));
        assert!(hdr_driver_allows_output_with_override(Some(0x1002), false));
        assert!(hdr_driver_allows_output_with_override(None, false));
    }

    #[test]
    fn parses_type1_hdr_metadata_from_edid() {
        let mut edid = [0_u8; 256];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        edid[25..35].copy_from_slice(&[0x7e, 0x45, 0xac, 0x4f, 0x47, 0xa6, 0x27, 0x12, 0x50, 0x54]);
        edid[126] = 1;
        edid[128] = 0x02;
        edid[130] = 11;
        edid[132..139].copy_from_slice(&[0xe6, 0x06, 0x07, 0x01, 0x61, 0x56, 0x1c]);

        let support = parse_edid_hdr_support(&edid);

        assert!(support.edid_hdr_static_metadata);
        assert!(support.edid_static_metadata_type1);
        assert!(support.edid_pq);
        assert!(!support.edid_hlg);
        assert_eq!(
            support.edid_hdr_metadata,
            Some(EdidHdrMetadata {
                display_primaries: [(33_643, 15_576), (14_014, 32_520), (7_666, 3_516)],
                white_point: (15_674, 16_455),
                max_luminance: 409,
                min_luminance: 493,
                max_fall: 322,
            })
        );
    }
}

fn drm_card_vendor_id(card_path: &Path) -> Option<u32> {
    let card_name = card_path.file_name()?.to_string_lossy();
    let vendor_path = PathBuf::from("/sys/class/drm")
        .join(card_name.as_ref())
        .join("device/vendor");
    let vendor = std::fs::read_to_string(vendor_path).ok()?;
    u32::from_str_radix(vendor.trim().trim_start_matches("0x"), 16).ok()
}

fn hdr_output_capable(
    hdr_support: &HdrSupport,
    hdr_offscreen_format: Option<Fourcc>,
    hdr_working_format: Option<Fourcc>,
    ten_bit_scanout_active: bool,
    gpu_vendor_id: Option<u32>,
) -> bool {
    hdr_support.can_enable(ten_bit_scanout_active)
        && hdr_offscreen_format.is_some()
        && hdr_working_format.is_some()
        && hdr_driver_allows_output(gpu_vendor_id)
}

mod hdr_output {
    use super::*;

    fn abort_hdr_rendering_userspace(surface: &mut DrmSurfaceState, reason: &str) {
        flog_warn!(
            "HDR aborted on {}: {reason} (userspace only; KMS unchanged — restart session for clean SDR scanout)",
            surface.output.name()
        );
        surface.hdr_transition_target = None;
        surface.hdr_render_supported = false;
        surface.frame_queued_at = None;
        surface.render_targets.offscreen = None;
        surface.render_targets.linear_offscreen = None;
        surface.render_targets.hdr_offscreen = None;
        surface.render_targets.encode_scratch = None;
        surface.render_targets.encoded_scanout = false;
        surface.render_targets.encoded_hdr = false;
    }

    pub(crate) fn stage_hdr_output_state(
        surface: &mut DrmSurfaceState,
        device: &impl drm::control::Device,
        hdr_target: bool,
    ) -> bool {
        if hdr_target == surface.hdr_enabled_applied {
            return true;
        }

        if !hdr_target {
            return match hdr_detection::hdr_kms::configure_smithay_hdr_state(
                surface, device, false, None,
            ) {
                Ok(true) => true,
                Ok(false) => {
                    flog_warn!(
                        "SDR connector state could not be queued on {}; will retry",
                        surface.output.name()
                    );
                    false
                }
                Err(err) => {
                    flog_warn!(
                        "Failed to queue SDR connector state through Smithay on {}: {err}; will retry",
                        surface.output.name()
                    );
                    false
                }
            };
        }

        hdr_detection::hdr_kms::log_connector_hdr_properties(
            device,
            surface.connector,
            &surface.output.name(),
            "before-queue",
        );

        let hdr_metadata_blob = match hdr_detection::hdr_kms::ensure_hdr_metadata_blob(
            device,
            &surface.hdr_support,
            &mut surface.hdr_metadata_blob,
        ) {
            Ok(blob) => Some(blob),
            Err(err) => {
                abort_hdr_rendering_userspace(
                    surface,
                    &format!("HDR metadata blob creation failed: {err}"),
                );
                return false;
            }
        };

        match hdr_detection::hdr_kms::configure_smithay_hdr_state(
            surface,
            device,
            true,
            hdr_metadata_blob,
        ) {
            Ok(true) => {}
            Ok(false) => {
                abort_hdr_rendering_userspace(surface, "HDR connector state could not be queued");
                return false;
            }
            Err(err) => {
                abort_hdr_rendering_userspace(
                    surface,
                    &format!("failed to queue HDR connector state through Smithay: {err}"),
                );
                return false;
            }
        }

        true
    }
}

pub(crate) fn collect_display_configs(
    device: &DrmDeviceState,
    core: &CompositorCore,
    configured_displays: &[DisplayConfig],
) -> Vec<DisplayConfig> {
    let mut displays = Vec::new();

    for (_crtc, surface) in &device.surfaces {
        let output_id = surface.output_id;

        let core_output = core.state.outputs.get(&output_id);

        let (scale, logical_x, logical_y, mut primary) = if let Some(o) = core_output {
            (
                o.scale_factor,
                o.logical_origin.x,
                o.logical_origin.y,
                core.state.primary_output == output_id,
            )
        } else {
            (1.0, 0, 0, false)
        };

        let w = surface.mode.size.w;
        let h = surface.mode.size.h;
        let hdr_supported = core_output
            .map(|output| output.hdr_supported)
            .unwrap_or(false);
        let hdr_requested = core_output
            .map(|output| output.hdr_requested)
            .unwrap_or(false);
        let hdr_enabled = core_output
            .map(|output| output.hdr_enabled)
            .unwrap_or(false);
        let output_name = surface.output.name();
        if configured_displays.iter().any(|display| display.primary) {
            primary = configured_displays
                .iter()
                .find(|display| display.name == output_name)
                .is_some_and(|display| display.primary);
        }
        let color_profile = configured_display_color_profile(configured_displays, &output_name);
        let icc_profile_path = configured_displays
            .iter()
            .find(|display| display.name == output_name)
            .and_then(|display| display.icc_profile_path.clone());

        displays.push(DisplayConfig {
            name: output_name,
            enabled: true,

            mode_width: w,
            mode_height: h,
            refresh_mhz: surface.mode.refresh,

            scale,

            logical_x,
            logical_y,

            physical_width_mm: None, // we’ll fix this next
            physical_height_mm: None,

            primary,

            transform: DisplayTransform::Normal,

            hdr_supported,
            hdr_requested,
            hdr_enabled,
            color_profile,
            icc_profile_path,
        });
    }

    merge_disconnected_display_configs(displays, configured_displays)
}

fn merge_disconnected_display_configs(
    mut displays: Vec<DisplayConfig>,
    configured_displays: &[DisplayConfig],
) -> Vec<DisplayConfig> {
    // Keep disconnected monitors in the file so a later replug can recover their
    // scale, position, primary choice, ICC profile, and HDR preference.
    let connected_names: std::collections::HashSet<_> = displays
        .iter()
        .map(|display| display.name.clone())
        .collect();
    for configured in configured_displays {
        if connected_names.contains(&configured.name) {
            continue;
        }
        let mut disconnected = configured.clone();
        disconnected.enabled = false;
        disconnected.hdr_enabled = false;
        displays.push(disconnected);
    }

    displays.sort_by_key(|display| (display.logical_x, display.logical_y, display.name.clone()));

    displays
}

/// Resolve which DRM node is primary for this seat (KMS node, matches udev
/// `device_list` entries). This must not open the device: `device_added` opens
/// it once through the active session.
fn primary_drm_node<S: Session>(session: &S) -> Result<DrmNode>
where
    S: Session,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let primary_path = primary_gpu(session.seat())?
        .ok_or_else(|| anyhow!("No primary GPU found for seat {}", session.seat()))?;

    DrmNode::from_path(&primary_path)
        .map_err(|e| anyhow!("Failed to create DrmNode from {:?}: {e}", primary_path))
}

#[derive(Debug, Clone)]
struct EdidMonitorIdentity {
    make: String,
    model: String,
    serial_number: String,
}

fn connector_edid(
    device: &impl drm::control::Device,
    connector: connector::Handle,
) -> Option<Vec<u8>> {
    let props = device.get_properties(connector).ok()?;
    for (prop, raw_value) in props.iter() {
        let info = device.get_property(*prop).ok()?;
        if info.name().to_bytes() == b"EDID" && *raw_value != 0 {
            return device.get_property_blob(*raw_value).ok();
        }
    }
    None
}

mod hdr_detection {
    use super::*;

    pub(crate) fn connector_hdr_support(
        device: &impl drm::control::Device,
        connector: connector::Handle,
        edid: Option<&[u8]>,
    ) -> HdrSupport {
        let mut support = edid
            .map(parse_edid_hdr_support)
            .unwrap_or_else(HdrSupport::default);

        let Ok(props) = device.get_properties(connector) else {
            return support;
        };

        for (prop, raw_value) in props.iter() {
            let Ok(info) = device.get_property(*prop) else {
                continue;
            };
            let name = info.name().to_string_lossy();

            match name.as_ref() {
                "HDR_OUTPUT_METADATA" => {
                    support.has_hdr_metadata_property = true;
                    support.hdr_metadata_blob = (*raw_value != 0).then_some(*raw_value);
                }
                "Colorspace" => {
                    if let drm::control::property::ValueType::Enum(values) = info.value_type() {
                        let (_, enums) = values.values();
                        support.colorspaces = enums
                            .iter()
                            .map(|value| value.name().to_string_lossy().into_owned())
                            .collect();
                        support.has_bt2020_colorspace = support
                            .colorspaces
                            .iter()
                            .any(|colorspace| colorspace.contains("BT2020"));
                        support.current_colorspace = values
                            .get_value_from_raw_value(*raw_value)
                            .map(|value| value.name().to_string_lossy().into_owned());
                    }
                }
                "max bpc" => {
                    if let drm::control::property::ValueType::UnsignedRange(min, max) =
                        info.value_type()
                    {
                        support.max_bpc = Some(HdrBpcRange { min, max });
                        support.current_max_bpc = Some(*raw_value);
                    }
                }
                _ => {}
            }
        }

        support
    }

    pub(crate) fn parse_edid_hdr_support(edid: &[u8]) -> HdrSupport {
        let mut support = HdrSupport::default();
        if edid.len() < 128
            || edid.get(0..8) != Some([0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0].as_slice())
        {
            return support;
        }

        let chromaticity = |high: u8, low: u8| -> u16 {
            let raw = (u16::from(high) << 2) | u16::from(low);
            ((u32::from(raw) * 50_000 + 512) / 1024) as u16
        };
        let chroma_low_1 = edid[25];
        let chroma_low_2 = edid[26];
        let display_primaries = [
            (
                chromaticity(edid[27], (chroma_low_1 >> 6) & 0x03),
                chromaticity(edid[28], (chroma_low_1 >> 4) & 0x03),
            ),
            (
                chromaticity(edid[29], (chroma_low_1 >> 2) & 0x03),
                chromaticity(edid[30], chroma_low_1 & 0x03),
            ),
            (
                chromaticity(edid[31], (chroma_low_2 >> 6) & 0x03),
                chromaticity(edid[32], (chroma_low_2 >> 4) & 0x03),
            ),
        ];
        let white_point = (
            chromaticity(edid[33], (chroma_low_2 >> 2) & 0x03),
            chromaticity(edid[34], chroma_low_2 & 0x03),
        );

        let extension_count = edid[126] as usize;
        for ext_index in 0..extension_count {
            let start = 128 * (ext_index + 1);
            let Some(extension) = edid.get(start..start + 128) else {
                break;
            };
            if extension[0] != 0x02 {
                continue;
            }

            let dtd_offset = match extension[2] {
                0 => 127,
                offset @ 4..=127 => offset as usize,
                _ => continue,
            };
            let mut index = 4;
            while index < dtd_offset {
                let header = extension[index];
                index += 1;

                let tag = header >> 5;
                let len = (header & 0x1f) as usize;
                if len == 0 || index + len > dtd_offset {
                    index += len;
                    continue;
                }

                let block = &extension[index..index + len];
                index += len;

                if tag != 0x07 || block.first() != Some(&0x06) || block.len() < 3 {
                    continue;
                }

                let eotf_flags = block[1];
                let supports_type1 = block[2] & 0x01 != 0;
                support.edid_hdr_static_metadata = true;
                support.edid_static_metadata_type1 |= supports_type1;
                support.edid_pq |= eotf_flags & (1 << 2) != 0;
                support.edid_hlg |= eotf_flags & (1 << 3) != 0;
                if !supports_type1 {
                    continue;
                }

                let max_luminance_nits = block
                    .get(3)
                    .map(|code| 50.0 * 2.0_f64.powf(f64::from(*code) / 32.0))
                    .unwrap_or(1_000.0);
                let max_fall_nits = block
                    .get(4)
                    .map(|code| 50.0 * 2.0_f64.powf(f64::from(*code) / 32.0))
                    .unwrap_or(max_luminance_nits);
                let min_luminance_nits = block
                    .get(5)
                    .map(|code| {
                        let ratio = f64::from(*code) / 255.0;
                        max_luminance_nits * ratio * ratio / 100.0
                    })
                    .unwrap_or(0.0);

                support.edid_hdr_metadata = Some(EdidHdrMetadata {
                    display_primaries,
                    white_point,
                    max_luminance: max_luminance_nits.round().clamp(1.0, f64::from(u16::MAX))
                        as u16,
                    min_luminance: (min_luminance_nits * 10_000.0)
                        .round()
                        .clamp(0.0, f64::from(u16::MAX)) as u16,
                    max_fall: max_fall_nits.round().clamp(1.0, f64::from(u16::MAX)) as u16,
                });
            }
        }

        support
    }

    pub(crate) fn log_hdr_support(output_name: &str, support: &HdrSupport) {
        let max_bpc = support
            .max_bpc
            .as_ref()
            .map(|range| format!("{}..{}", range.min, range.max))
            .unwrap_or_else(|| "none".to_string());
        let current_bpc = support
            .current_max_bpc
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let current_colorspace = support.current_colorspace.as_deref().unwrap_or("unknown");

        flog(&format!(
            "HDR support: output={} metadata_property={} metadata_blob={:?} bt2020_colorspace={} colorspaces={:?} current_colorspace={} max_bpc={} current_max_bpc={} edid_hdr_static_metadata={} edid_static_metadata_type1={} edid_pq={} edid_hlg={} edid_hdr_metadata={:?}",
            output_name,
            support.has_hdr_metadata_property,
            support.hdr_metadata_blob,
            support.has_bt2020_colorspace,
            support.colorspaces,
            current_colorspace,
            max_bpc,
            current_bpc,
            support.edid_hdr_static_metadata,
            support.edid_static_metadata_type1,
            support.edid_pq,
            support.edid_hlg,
            support.edid_hdr_metadata,
        ));
    }

    pub(crate) mod hdr_kms {
        use super::*;

        #[derive(Debug)]
        pub(crate) struct ConnectorHdrSnapshot {
            colorspace: Option<String>,
            max_bpc: Option<u64>,
            metadata_blob: Option<u64>,
        }

        impl ConnectorHdrSnapshot {
            fn validate(
                &self,
                hdr_enabled: bool,
                require_max_bpc_readback: bool,
            ) -> Result<(), anyhow::Error> {
                let colorspace = self
                    .colorspace
                    .as_deref()
                    .ok_or_else(|| anyhow!("connector Colorspace property is missing"))?;
                match self.max_bpc {
                    Some(max_bpc) if max_bpc < 10 => {
                        return Err(anyhow!(
                            "connector max bpc read back as {max_bpc}, expected at least 10"
                        ));
                    }
                    None if require_max_bpc_readback => {
                        return Err(anyhow!("connector max bpc property is missing"));
                    }
                    _ => {}
                }

                let metadata_blob = self
                    .metadata_blob
                    .ok_or_else(|| anyhow!("connector HDR_OUTPUT_METADATA property is missing"))?;
                if hdr_enabled {
                    if !colorspace.contains("BT2020") {
                        return Err(anyhow!(
                            "connector Colorspace read back as {colorspace}, expected BT2020"
                        ));
                    }
                    if metadata_blob == 0 {
                        return Err(anyhow!("connector HDR metadata read back as disabled"));
                    }
                } else {
                    if colorspace.contains("BT2020") {
                        return Err(anyhow!(
                            "connector Colorspace remained {colorspace} after SDR transition"
                        ));
                    }
                    if metadata_blob != 0 {
                        return Err(anyhow!(
                            "connector HDR metadata blob {metadata_blob} remained active after SDR transition"
                        ));
                    }
                }
                Ok(())
            }
        }

        pub(crate) fn validate_connector_hdr_state(
            device: &impl drm::control::Device,
            connector: connector::Handle,
            hdr_enabled: bool,
            require_max_bpc_readback: bool,
        ) -> Result<ConnectorHdrSnapshot, anyhow::Error> {
            let props = device.get_properties(connector).map_err(|err| {
                anyhow!("failed to read connector properties after HDR commit: {err}")
            })?;
            let mut snapshot = ConnectorHdrSnapshot {
                colorspace: None,
                max_bpc: None,
                metadata_blob: None,
            };

            for (prop, raw_value) in props.iter() {
                let Ok(info) = device.get_property(*prop) else {
                    continue;
                };
                match info.name().to_string_lossy().as_ref() {
                    "Colorspace" => {
                        if let property::ValueType::Enum(values) = info.value_type() {
                            snapshot.colorspace = values
                                .get_value_from_raw_value(*raw_value)
                                .map(|value| value.name().to_string_lossy().into_owned());
                        }
                    }
                    "max bpc" => snapshot.max_bpc = Some(*raw_value),
                    "HDR_OUTPUT_METADATA" => snapshot.metadata_blob = Some(*raw_value),
                    _ => {}
                }
            }

            snapshot.validate(hdr_enabled, require_max_bpc_readback)?;
            Ok(snapshot)
        }

        pub(crate) fn log_connector_hdr_properties(
            device: &impl drm::control::Device,
            connector: connector::Handle,
            output_name: &str,
            phase: &str,
        ) {
            let props = match device.get_properties(connector) {
                Ok(props) => props,
                Err(err) => {
                    flog(&format!(
                        "HDR KMS properties: output={output_name} connector={connector:?} phase={phase} read_error={err}"
                    ));
                    return;
                }
            };

            let mut colorspace = "missing".to_string();
            let mut max_bpc = "missing".to_string();
            let mut metadata = "missing".to_string();

            for (prop, raw_value) in props.iter() {
                let Ok(info) = device.get_property(*prop) else {
                    continue;
                };

                match info.name().to_string_lossy().as_ref() {
                    "Colorspace" => {
                        colorspace = match info.value_type() {
                            property::ValueType::Enum(values) => values
                                .get_value_from_raw_value(*raw_value)
                                .map(|value| {
                                    format!("{}({raw_value})", value.name().to_string_lossy())
                                })
                                .unwrap_or_else(|| format!("unknown({raw_value})")),
                            _ => format!("raw({raw_value})"),
                        };
                    }
                    "max bpc" => max_bpc = raw_value.to_string(),
                    "HDR_OUTPUT_METADATA" => {
                        metadata = if *raw_value == 0 {
                            "none(0)".to_string()
                        } else {
                            format!("blob({raw_value})")
                        };
                    }
                    _ => {}
                }
            }

            flog(&format!(
                "HDR KMS properties: output={output_name} connector={connector:?} phase={phase} colorspace={colorspace} max_bpc={max_bpc} metadata={metadata}"
            ));
        }

        fn hdr_point(x: u16, y: u16) -> drm_ffi::hdr_metadata_infoframe__bindgen_ty_1 {
            drm_ffi::hdr_metadata_infoframe__bindgen_ty_1 { x, y }
        }

        fn hdr_white_point(x: u16, y: u16) -> drm_ffi::hdr_metadata_infoframe__bindgen_ty_2 {
            drm_ffi::hdr_metadata_infoframe__bindgen_ty_2 { x, y }
        }

        fn build_hdr_output_metadata(support: &HdrSupport) -> drm_ffi::hdr_output_metadata {
            const DRM_MODE_HDR_METADATA_TYPE1: u32 = 0;
            const HDMI_EOTF_SMPTE_ST2084: u8 = 2;
            // HDMI Static Metadata Descriptor Type 1 is encoded as descriptor ID zero.
            const HDMI_STATIC_METADATA_TYPE1: u8 = 0;

            let metadata = support
                .edid_hdr_metadata
                .expect("HDR metadata blob requires parsed EDID Type 1 metadata");
            let max_luminance = crate::core::color::hdr10_kms_max_luminance_nits();
            let max_cll = crate::core::color::hdr10_kms_max_cll_nits();
            let max_fall = crate::core::color::hdr10_kms_max_fall_nits();

            let infoframe = drm_ffi::hdr_metadata_infoframe {
                eotf: HDMI_EOTF_SMPTE_ST2084,
                metadata_type: HDMI_STATIC_METADATA_TYPE1,
                display_primaries: metadata.display_primaries.map(|(x, y)| hdr_point(x, y)),
                white_point: hdr_white_point(metadata.white_point.0, metadata.white_point.1),
                max_display_mastering_luminance: max_luminance,
                min_display_mastering_luminance: metadata.min_luminance,
                max_cll,
                max_fall,
            };

            drm_ffi::hdr_output_metadata {
                metadata_type: DRM_MODE_HDR_METADATA_TYPE1,
                __bindgen_anon_1: drm_ffi::hdr_output_metadata__bindgen_ty_1 {
                    hdmi_metadata_type1: infoframe,
                },
            }
        }

        pub(crate) fn create_hdr_metadata_blob(
            device: &impl drm::control::Device,
            support: &HdrSupport,
        ) -> Result<u64, anyhow::Error> {
            if !support.can_signal_hdr10() {
                return Err(anyhow!(
                    "HDR metadata requested without complete HDR support"
                ));
            }

            match device
                .create_property_blob(&build_hdr_output_metadata(support))
                .map_err(|err| anyhow!("failed to create HDR metadata blob: {err}"))?
            {
                property::Value::Blob(blob) => Ok(blob),
                other => Err(anyhow!(
                    "DRM returned non-blob value for HDR metadata: {other:?}"
                )),
            }
        }

        pub(crate) fn destroy_hdr_metadata_blob(
            device: &impl drm::control::Device,
            blob: Option<u64>,
        ) {
            if let Some(blob) = blob {
                if let Err(err) = device.destroy_property_blob(blob) {
                    flog(&format!(
                        "Failed to destroy HDR metadata blob {blob}: {err}"
                    ));
                }
            }
        }

        pub(crate) fn ensure_hdr_metadata_blob(
            device: &impl drm::control::Device,
            support: &HdrSupport,
            blob: &mut Option<u64>,
        ) -> Result<u64, anyhow::Error> {
            if let Some(blob) = *blob {
                return Ok(blob);
            }

            let created = create_hdr_metadata_blob(device, support)?;
            *blob = Some(created);
            flog(&format!(
                "HDR10 KMS metadata blob={created} max_luminance={} max_cll={} max_fall={} (SDR white) min_mastering={:?}",
                crate::core::color::hdr10_kms_max_luminance_nits(),
                crate::core::color::hdr10_kms_max_cll_nits(),
                crate::core::color::hdr10_kms_max_fall_nits(),
                support
                    .edid_hdr_metadata
                    .map(|metadata| metadata.min_luminance)
            ));
            Ok(created)
        }

        fn select_colorspace_value(
            info: &property::Info,
            _support: &HdrSupport,
            hdr_enabled: bool,
        ) -> Option<u64> {
            let property::ValueType::Enum(values) = info.value_type() else {
                return None;
            };

            let (_, enums) = values.values();
            let selected = if hdr_enabled {
                enums
                    .iter()
                    .find(|value| value.name().to_string_lossy().contains("BT2020_RGB"))
                    .or_else(|| {
                        enums
                            .iter()
                            .find(|value| value.name().to_string_lossy().contains("BT2020"))
                    })
            } else {
                enums
                    .iter()
                    .find(|value| value.name().to_string_lossy() == "Default")
                    .or_else(|| {
                        enums
                            .iter()
                            .find(|value| value.name().to_string_lossy().contains("BT709"))
                    })
                    .or_else(|| {
                        enums.iter().find(|value| {
                            let name = value.name().to_string_lossy();
                            name.contains("RGB") && !name.contains("BT2020")
                        })
                    })
                    .or_else(|| enums.first())
            };

            selected.map(|value| value.value())
        }

        fn build_connector_hdr_state(
            device: &impl drm::control::Device,
            connector: connector::Handle,
            support: &HdrSupport,
            hdr_enabled: bool,
            hdr_metadata_blob: Option<u64>,
        ) -> Result<Option<HdrState>, anyhow::Error> {
            if hdr_enabled && (!support.can_signal_hdr10() || hdr_metadata_blob.is_none()) {
                return Ok(None);
            }

            if !hdr_enabled && !support.has_connector_controls() {
                return Ok(None);
            }

            let props = device
                .get_properties(connector)
                .map_err(|err| anyhow!("failed to read connector properties for HDR: {err}"))?;

            let mut state = HdrState::default();
            let mut changed = false;

            for (prop, raw_value) in props.iter() {
                let info = match device.get_property(*prop) {
                    Ok(info) => info,
                    Err(_) => continue,
                };
                let name = info.name().to_string_lossy();

                match name.as_ref() {
                    "max bpc" => {
                        if support.bpc_control_allows_ten_bit() {
                            // Do not reduce a link that was already configured
                            // above 10 bpc, but raise an 8-bpc default as part of
                            // the same atomic transaction as colorspace and HDR
                            // metadata. A 10-bit framebuffer alone does not set
                            // the DisplayPort link depth.
                            state.max_bpc = Some(
                                support
                                    .current_max_bpc
                                    .unwrap_or(10)
                                    .max(10)
                                    .min(support.max_bpc.as_ref().unwrap().max),
                            );
                            changed = true;
                        }
                    }
                    "Colorspace" => {
                        if let Some(value) = select_colorspace_value(&info, support, hdr_enabled) {
                            state.colorspace = Some(value);
                            changed = true;
                        }
                    }
                    "HDR_OUTPUT_METADATA" => {
                        if let property::ValueType::Blob = info.value_type() {
                            let target = if hdr_enabled {
                                hdr_metadata_blob.unwrap_or(*raw_value)
                            } else {
                                0
                            };
                            state.hdr_output_metadata = Some(property::Value::Blob(target));
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }

            if !changed {
                return Ok(None);
            }

            Ok(Some(state))
        }

        pub(crate) fn configure_smithay_hdr_state(
            surface: &DrmSurfaceState,
            device: &impl drm::control::Device,
            hdr_enabled: bool,
            hdr_metadata_blob: Option<u64>,
        ) -> Result<bool, anyhow::Error> {
            let Some(state) = build_connector_hdr_state(
                device,
                surface.connector,
                &surface.hdr_support,
                hdr_enabled,
                hdr_metadata_blob,
            )?
            else {
                return Ok(true);
            };

            surface
                .drm_output
                .with_compositor(|compositor| compositor.use_hdr_state(state))
                .map_err(|err| anyhow!("failed to queue HDR connector state: {err}"))?;

            Ok(true)
        }

        #[cfg(test)]
        mod tests {
            use super::ConnectorHdrSnapshot;

            #[test]
            fn validates_complete_hdr_property_readback() {
                let snapshot = ConnectorHdrSnapshot {
                    colorspace: Some("BT2020_RGB".into()),
                    max_bpc: Some(10),
                    metadata_blob: Some(42),
                };
                assert!(snapshot.validate(true, true).is_ok());
            }

            #[test]
            fn rejects_partial_hdr_property_readback() {
                let eight_bit = ConnectorHdrSnapshot {
                    colorspace: Some("BT2020_RGB".into()),
                    max_bpc: Some(8),
                    metadata_blob: Some(42),
                };
                assert!(eight_bit.validate(true, true).is_err());

                let missing_metadata = ConnectorHdrSnapshot {
                    colorspace: Some("BT2020_RGB".into()),
                    max_bpc: Some(10),
                    metadata_blob: Some(0),
                };
                assert!(missing_metadata.validate(true, true).is_err());
            }

            #[test]
            fn accepts_driver_managed_bpc_after_ten_bit_scanout_probe() {
                let snapshot = ConnectorHdrSnapshot {
                    colorspace: Some("BT2020_RGB".into()),
                    max_bpc: None,
                    metadata_blob: Some(42),
                };
                assert!(snapshot.validate(true, false).is_ok());
                assert!(snapshot.validate(true, true).is_err());
            }

            #[test]
            fn validates_sdr_rollback_readback() {
                let snapshot = ConnectorHdrSnapshot {
                    colorspace: Some("Default".into()),
                    max_bpc: Some(10),
                    metadata_blob: Some(0),
                };
                assert!(snapshot.validate(false, true).is_ok());
            }
        }
    }
}

fn parse_edid_identity(edid: &[u8]) -> Option<EdidMonitorIdentity> {
    if edid.len() < 128 || edid.get(0..8)? != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return None;
    }

    let manufacturer = u16::from_be_bytes([edid[8], edid[9]]);
    let make = [
        (((manufacturer >> 10) & 0x1f) as u8 + b'A' - 1) as char,
        (((manufacturer >> 5) & 0x1f) as u8 + b'A' - 1) as char,
        ((manufacturer & 0x1f) as u8 + b'A' - 1) as char,
    ]
    .iter()
    .collect::<String>();

    let product_code = u16::from_le_bytes([edid[10], edid[11]]);
    let numeric_serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    let mut monitor_name = None;
    let mut descriptor_serial = None;

    for descriptor in edid[54..126].chunks_exact(18) {
        if descriptor[0..3] != [0, 0, 0] {
            continue;
        }

        match descriptor[3] {
            0xfc => monitor_name = edid_descriptor_text(descriptor),
            0xff => descriptor_serial = edid_descriptor_text(descriptor),
            _ => {}
        }
    }

    let model = monitor_name.unwrap_or_else(|| format!("0x{product_code:04x}"));
    let serial_number = descriptor_serial.unwrap_or_else(|| {
        if numeric_serial != 0 {
            numeric_serial.to_string()
        } else {
            "unknown".to_string()
        }
    });

    Some(EdidMonitorIdentity {
        make,
        model,
        serial_number,
    })
}

fn edid_descriptor_text(descriptor: &[u8]) -> Option<String> {
    let text = descriptor.get(5..18)?;
    let end = text
        .iter()
        .position(|byte| matches!(*byte, b'\n' | b'\r' | 0))
        .unwrap_or(text.len());
    let value = String::from_utf8_lossy(&text[..end]).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Maps an xkb-offset `KEY_F1..KEY_F12` code to a target VT number, for the
/// Ctrl+Alt+F<n> "drop to console" shortcut. `key_code()` on smithay's
/// `KeyboardKeyEvent` (libinput backend) returns the raw evdev code `+8`
/// (see `smithay::backend::libinput`'s `KeyboardKeyEvent::key_code` impl),
/// not the raw evdev code itself, so the evdev F1..F12 values (59..=68, 87, 88)
/// must be shifted by 8 here (67..=76, 95, 96).
fn vt_switch_target(keycode: u32) -> Option<i32> {
    match keycode {
        67..=76 => Some((keycode - 67 + 1) as i32), // KEY_F1..KEY_F10 -> vt 1..10
        95 => Some(11),                             // KEY_F11 -> vt 11
        96 => Some(12),                             // KEY_F12 -> vt 12
        _ => None,
    }
}

fn dispatch_backend_input_event<B: smithay::backend::input::InputBackend>(
    state: &mut DesktopState,
    input: &smithay::backend::input::InputEvent<B>,
) {
    use smithay::backend::input::InputEvent;

    let output_id = state
        .output_at_logical_point(state.pointer_pos)
        .unwrap_or(state.focused_output);
    let scale = state
        .outputs
        .get(&output_id)
        .map(|o| o.scale_factor)
        .unwrap_or(1.0);

    // Absolute devices (touchpad/tablet in absolute mode) must transform against the
    // focused output's geometry, not the combined desktop bounds (anvil/smallvil pattern).
    let clamp_rect = match input {
        InputEvent::PointerMotionAbsolute { .. } => {
            state.pointer_transform_rect_for_output(output_id)
        }
        _ => state.logical_pointer_clamp_rect(),
    };

    if let Some(mut event) = translate_backend_input(
        input,
        state.input.pointer_pos,
        clamp_rect,
        scale,
        state.input.modifiers,
    ) {
        if matches!(input, InputEvent::PointerMotion { .. }) {
            if let crate::core::input::FlowInputEvent::PointerMoved { position, .. } = &mut event {
                let scale = scale.max(1.0);
                let old_pos = state.input.pointer_pos;
                let scaled = Point::<f64, Logical>::from((
                    old_pos.x + (position.x - old_pos.x) / scale,
                    old_pos.y + (position.y - old_pos.y) / scale,
                ));
                let min_x = clamp_rect.loc.x as f64;
                let min_y = clamp_rect.loc.y as f64;
                let max_x = (clamp_rect.loc.x + clamp_rect.size.w) as f64 - f64::EPSILON;
                let max_y = (clamp_rect.loc.y + clamp_rect.size.h) as f64 - f64::EPSILON;
                *position = Point::from((
                    scaled.x.clamp(min_x, max_x.max(min_x)),
                    scaled.y.clamp(min_y, max_y.max(min_y)),
                ));
            }
        }
        state.handle_input(event);
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    flog("FOCALDESK: entered DRM backend");
    let mut event_loop: EventLoop<DrmLoopData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    //
    // Session / seat ownership
    //
    let (session, notifier) =
        LibSeatSession::new().map_err(|e| anyhow!("Could not initialize libseat session: {e}"))?;

    //
    // Shared compositor state
    //
    let desktop = bootstrap_compositor_core(None, BackendKind::Drm)?;

    //
    // Pick primary KMS device (same node udev will report for the card — open only in `device_added`).
    //
    let primary_node = primary_drm_node(&session)?;
    flog(&format!(
        "Primary DRM node for seat {}: {:?}",
        session.seat(),
        primary_node
    ));

    //
    // libinput
    //
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|e| anyhow!("Failed to assign libinput seat: {:?}", e))?;

    let libinput_backend = LibinputInputBackend::new(libinput.clone());

    //
    // udev
    //
    let udev = UdevBackend::new(session.seat())
        .map_err(|e| anyhow!("Failed to initialize udev backend: {e}"))?;

    #[cfg(feature = "xwayland")]
    let xwayland_event_loop = EventLoop::<DesktopState>::try_new()?;

    let mut data = DrmLoopData {
        core: CompositorCore {
            display: desktop.display,
            #[cfg(feature = "xwayland")]
            xwayland_event_loop,
            listener: desktop.listener,
            wayland_display: desktop.wayland_display,
            state: desktop.state,
            clients: desktop.clients,
            ui_state: desktop.ui_state,
            scene: desktop.scene,
            output_state: desktop.output_state,
            resources: desktop.resources,
            start: desktop.start,
            last_now: desktop.last_now,
        },
        backend: DrmBackend {
            session: session.clone(),
            primary_gpu: primary_node,
            devices: HashMap::new(),
        },
        libinput,
        session_active: session.is_active(),
        resume_pending: false,
        resume_retry_at: None,
        exclusive_hdr_recovery_nodes: Vec::new(),
        should_stop: false,
    };

    let _libinput_token = loop_handle.insert_source(libinput_backend, |event, _, data| {
        if let InputEvent::Keyboard { event, .. } = &event {
            let keycode = event.key_code();
            let key_state = event.state();
            flog(&format!(
                "key event: code={:?} state={:?}",
                keycode, key_state
            ));

            if key_state == KeyState::Pressed {
                let mods = data.core.state.input.modifiers;
                if mods.ctrl && mods.alt {
                    if let Some(vt) = vt_switch_target(keycode.into()) {
                        flog(&format!("VT switch requested: ctrl+alt+F -> vt{vt}"));
                        if let Err(err) = data.backend.session.change_vt(vt) {
                            flog_warn!("VT switch to {vt} failed: {err:?}");
                        }
                        return;
                    }
                }
            }
        }

        if let InputEvent::SwitchToggle { event, .. } = &event {
            if matches!(event.switch(), Some(InputSwitch::Lid)) {
                let closed = matches!(event.state(), SwitchState::On);
                data.core.state.handle_lid_switch(closed);
            }
        }

        dispatch_backend_input_event::<LibinputInputBackend>(&mut data.core.state, &event);
    })?;

    let (colord_tx, colord_rx) = calloop::channel::channel();
    if let Err(err) = crate::core::colord::spawn_colord_watch(move || {
        let _ = colord_tx.send(());
    }) {
        flog(format!("colord watch thread failed to start: {err:?}"));
    } else {
        let _colord_token = loop_handle.insert_source(colord_rx, |event, _, data| {
            if let calloop::channel::Event::Msg(()) = event {
                if crate::core::colord::refresh_all_output_colors(&mut data.core.state) {
                    flog("colord: output color profiles refreshed");
                }
            }
        })?;
    }

    let _session_token =
        loop_handle.insert_source(notifier, move |event, _, data| match event {
            SessionEvent::PauseSession => {
                pause_drm_session(data, "libseat PauseSession");
            }
            SessionEvent::ActivateSession => {
                resume_drm_session(data, "libseat ActivateSession");
            }
        })?;
    let sleep_notifications = spawn_session_sleep_watch().ok();

    for (device_id, path) in udev.device_list() {
        let node = DrmNode::from_dev_id(device_id)
            .map_err(|e| anyhow!("Failed to build DrmNode from dev id {device_id:?}: {e}"))?;

        if node != primary_node {
            flog(&format!("Skipping non-primary DRM node {}", path.display()));
            continue;
        }

        device_added(&mut data, &loop_handle, node, &path)?;
    }

    let udev_handle = loop_handle.clone();
    let _udev_token = loop_handle.insert_source(udev, move |event, _, data| match event {
        UdevEvent::Added { device_id, path } => {
            let Ok(node) = DrmNode::from_dev_id(device_id) else {
                return;
            };
            if node == data.backend.primary_gpu && !data.backend.devices.contains_key(&node) {
                if let Err(err) = device_added(data, &udev_handle, node, &path) {
                    flog(&format!(
                        "Failed to add DRM device {}: {err}",
                        path.display()
                    ));
                }
            }
        }
        UdevEvent::Changed { device_id } => {
            let Ok(node) = DrmNode::from_dev_id(device_id) else {
                return;
            };
            let topology_changed = data
                .backend
                .devices
                .get(&node)
                .map(|device| drm_connector_topology_changed(device, &data.core.state));
            if matches!(topology_changed, Some(Ok(true))) {
                flog(&format!(
                    "DRM connector topology changed on {node:?}; rebuilding outputs"
                ));
                if let Err(err) = reinitialize_drm_device(data, &udev_handle, node) {
                    flog(&format!("Failed to rebuild outputs after hotplug: {err}"));
                }
            } else if let Some(device) = data.backend.devices.get_mut(&node) {
                if let Some(Err(err)) = topology_changed {
                    flog(&format!("Failed to inspect changed DRM device: {err}"));
                }
                if data.session_active {
                    if let Err(err) = device.drm_output_manager.lock().activate(false) {
                        flog(&format!("Failed to refresh changed DRM device: {err}"));
                    }
                }
                data.core
                    .state
                    .mark_all_outputs_full_damage(DamageSource::Unknown);
                data.core.state.mark_redraw();
            }
        }
        UdevEvent::Removed { device_id } => {
            let Ok(node) = DrmNode::from_dev_id(device_id) else {
                return;
            };
            remove_drm_device(data, &udev_handle, node);
        }
    })?;

    #[cfg(feature = "xwayland")]
    {
        start_xwayland(
            &mut data.core.state,
            &data.core.display.handle(),
            data.core.xwayland_event_loop.handle(),
        )?;
        finish_xwayland_startup(
            &mut data.core.xwayland_event_loop,
            &mut data.core.display,
            &mut data.core.state,
            Duration::from_secs(30),
        )?;
        if let Some(display) = data.core.state.xwayland_display.as_deref() {
            flog(&format!(
                "DRM backend: XWayland active on DISPLAY={display}"
            ));
        } else {
            flog("DRM backend: XWayland is not active (startup failed or disabled)");
        }
    }

    //let (renderer, mut framebuffer) = acquire_drm_framebuffer(...)?
    //let prepared = prepare_active_output(...)?;
    //let mut frame = renderer.render(&mut framebuffer, buffer_size, Transform::Normal /* or whatever is correct */)?;
    //draw_active_output(...)?;
    //frame.finish()?;
    //present_drm_framebuffer(...)?

    //
    // Main loop
    //
    while !data.should_stop && data.core.state.running {
        if let Some(rx) = sleep_notifications.as_ref() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    SessionSleepEvent::GoingToSleep => {
                        pause_drm_session(&mut data, "login1 PrepareForSleep(true)");
                    }
                    SessionSleepEvent::WokeUp => {
                        resume_drm_session(&mut data, "login1 PrepareForSleep(false)");
                    }
                }
            }
        }

        if data.resume_pending
            && data
                .resume_retry_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            resume_drm_session(&mut data, "deferred resume retry");
        }

        #[cfg(feature = "xwayland")]
        data.core
            .xwayland_event_loop
            .dispatch(Some(Duration::ZERO), &mut data.core.state)?;

        data.core.state.process_settings_ipc_requests();
        data.core.state.process_clipboard_captures();
        data.core.state.process_chrome_timers();
        data.core.state.process_notification_timers();
        data.core.state.process_idle_timers();
        data.core.state.process_power_timers();
        data.core.state.process_media_device_timers();
        data.core.state.process_network_state_timers();
        data.core.state.process_update_state_timers();
        data.core.state.process_lock_timers();

        event_loop.dispatch(Some(Duration::from_millis(16)), &mut data)?;
        let exclusive_recovery_nodes = std::mem::take(&mut data.exclusive_hdr_recovery_nodes);
        for node in exclusive_recovery_nodes {
            flog_warn!(
                "Rebuilding DRM device {node:?} to restore all outputs after exclusive HDR failure"
            );
            if let Err(err) = reinitialize_drm_device(&mut data, &loop_handle, node) {
                flog_warn!("Failed to restore SDR output topology on {node:?}: {err}");
            }
        }
        data.core.state.process_hdr_safe_session_action();
        data.core.state.process_deferred_ui_and_launches();

        if !data.session_active {
            continue;
        }

        let accept_started = Instant::now();
        if let Some(stream) = data.core.listener.accept()? {
            let client_state = client_state_from_stream(&stream);
            let client = data
                .core
                .display
                .handle()
                .insert_client(stream, std::sync::Arc::new(client_state))?;
            data.core.clients.push(client);
            flog(format!(
                "wayland accept elapsed_ms={} clients={}",
                accept_started.elapsed().as_millis(),
                data.core.clients.len()
            ));
        }

        let now = Instant::now();
        let dt = now.saturating_duration_since(data.core.last_now);
        {
            let core = &mut data.core;
            let backend = &mut data.backend;

            if let Some(device) = backend.devices.values_mut().next() {
                core.state.begin_portal_dispatch(
                    &mut device.renderer,
                    &mut core.ui_state,
                    &mut core.scene,
                    &core.output_state,
                    now,
                    dt,
                );
            }
        }
        if data.core.state.wayland_clients_may_dispatch() {
            let dispatch_started = Instant::now();
            if let Err(err) = data.core.display.dispatch_clients(&mut data.core.state) {
                if !is_nonfatal_wayland_io_error(&err) {
                    return Err(err.into());
                }
                flog_warn!("ignoring nonfatal Wayland dispatch error: {err}");
            }
            crate::core::wayland::color_management_protocol::flush_pending_image_description_info_done(
                &mut data.core.state,
            );
            flog(format!(
                "wayland dispatch_clients elapsed_ms={}",
                dispatch_started.elapsed().as_millis()
            ));
        }
        data.core.state.process_deferred_window_ops();
        data.core.state.end_portal_dispatch();

        data.core.state.refresh_space();
        if let Err(err) = data.core.display.handle().flush_clients() {
            if !is_nonfatal_wayland_io_error(&err) {
                return Err(err.into());
            }
            flog_warn!("ignoring nonfatal Wayland flush error: {err}");
        }
        data.core.state.tick_layout();

        let screenshot_output = data.core.state.screenshot_request();
        data.core
            .state
            .image_copy_capture_sessions
            .retain(|session| session.alive());
        let portal_pending = crate::core::portal::portal_capture_pending(&data.core.state);
        let portal_needs_composite = crate::core::portal::portal_needs_composite(&data.core.state);
        // A DRM page-flip event is delivered only for a frame that we actually
        // submit.  Merely damaging the output from the previous vblank callback
        // is not sufficient to keep a static desktop rendering: the retained
        // renderer may already have made its render/no-render decision for that
        // dispatch.  Explicitly carry verification into the main-loop render
        // decision and damage the selected output until the bounded PQ window
        // completes.
        let verifying_hdr_outputs: Vec<_> = data
            .backend
            .devices
            .values()
            .flat_map(|device| {
                let exclusive_hdr_output = device.exclusive_hdr_output.as_deref();
                device.surfaces.values().filter_map(move |surface| {
                    let is_exclusive = exclusive_hdr_output == Some(surface.output.name().as_str());
                    (surface.hdr_enabled_applied
                        && is_exclusive
                        && surface.hdr_verify_started_at.is_some())
                    .then_some(surface.output_id)
                })
            })
            .collect();
        for output_id in &verifying_hdr_outputs {
            data.core
                .state
                .mark_output_full_damage(*output_id, DamageSource::Unknown);
        }
        let should_render = data.core.state.needs_redraw()
            || screenshot_output.is_some()
            || data.core.state.screenshot_all_requested
            || portal_needs_composite
            || !verifying_hdr_outputs.is_empty();

        // Watchdog for any stalled DRM frame: runs every tick regardless of `should_render`,
        // since a frozen output produces no further damage to wake it up.
        // Some drivers accept the TEST_ONLY commit that precedes an HDR transition but then
        // never deliver the completion event for the real (NONBLOCK) commit, leaving
        // `frame_queued_at` set forever and that CRTC skipped on every future tick. Recovering
        // requires a fresh `DrmCompositor` for the device (Smithay has no public API to drop
        // just the stuck one), so we fall back to the same full-device reinit already used
        // after a failed session resume.
        let mut stalled_nodes: Vec<DrmNode> = Vec::new();
        for (node, device) in data.backend.devices.iter_mut() {
            let exclusive_hdr_output = device.exclusive_hdr_output.clone();
            let nvidia_dual = nvidia_dual_head_hdr_allowed();
            for surface in device.surfaces.values_mut() {
                // The HDR deadline is armed when connector state changes, before
                // render_frame/queue_frame. Keep watching it even when
                // that first HDR frame fails or is empty and frame_queued_at stays None.
                let hdr_stalled = hdr_commit_stalled(surface.hdr_commit_deadline, now);
                let frame_stalled = surface
                    .frame_queued_at
                    .is_some_and(|queued_at| queued_frame_stalled(queued_at, now));
                if !hdr_stalled && !frame_stalled {
                    continue;
                }
                if hdr_stalled {
                    let disabling = surface.hdr_transition_target == Some(false)
                        || data
                            .core
                            .state
                            .outputs
                            .get(&surface.output_id)
                            .is_some_and(|output| !output.hdr_requested);
                    if disabling {
                        flog_warn!(
                            "HDR disable commit stalled on {} past {:?}; recovering scanout without latching exclusive failure",
                            surface.output.name(),
                            HDR_FRAME_TIMEOUT
                        );
                    } else {
                        flog_warn!(
                            "HDR commit stalled on {} past {:?} with no vblank; disabling HDR and reinitializing the device to recover scanout",
                            surface.output.name(),
                            HDR_FRAME_TIMEOUT
                        );
                        let persist = hdr_failure_persist_action(
                            exclusive_hdr_output.as_deref(),
                            surface.output.name().as_str(),
                            nvidia_dual,
                        );
                        apply_persisted_hdr_failure(persist, surface.output.name().as_str());
                        if persist == HdrFailurePersist::DisableAll {
                            for output in data.core.state.outputs.values_mut() {
                                clear_runtime_hdr_request(output);
                            }
                        } else if let Some(output) =
                            data.core.state.outputs.get_mut(&surface.output_id)
                        {
                            clear_runtime_hdr_request(output);
                        }
                    }
                } else {
                    flog_warn!(
                        "DRM frame stalled on {} past {:?} with no vblank; reinitializing the device to recover scanout",
                        surface.output.name(),
                        DRM_FRAME_TIMEOUT
                    );
                }
                let disabling = surface.hdr_transition_target == Some(false)
                    || data
                        .core
                        .state
                        .outputs
                        .get(&surface.output_id)
                        .is_some_and(|output| !output.hdr_requested);
                if !disabling
                    && device.exclusive_hdr_output.as_deref()
                        == Some(surface.output.name().as_str())
                {
                    let reason = if hdr_stalled {
                        "HDR KMS transition timed out without a vblank"
                    } else {
                        "HDR verification frame timed out"
                    };
                    persist_exclusive_hdr_phase(
                        ExclusiveHdrPhase::Failed,
                        Some(surface.output.name().as_str()),
                        Some(reason),
                    );
                }
                surface.hdr_commit_deadline = None;
                surface.hdr_transition_target = None;
                surface.hdr_initial_modeset_pending = false;
                if let Some(output) = data.core.state.outputs.get_mut(&surface.output_id) {
                    output.hdr_transition_target = None;
                }
                if !stalled_nodes.contains(node) {
                    stalled_nodes.push(*node);
                }
            }
        }
        for node in stalled_nodes {
            if let Err(err) = reinitialize_drm_device(&mut data, &loop_handle, node) {
                flog_warn!(
                    "Failed to reinitialize DRM device {:?} after stalled frame: {err}",
                    node
                );
            }
        }

        if !should_render {
            if portal_pending {
                if let Some(device) = data.backend.devices.values_mut().next() {
                    crate::core::portal::complete_pending_portal_captures(
                        &mut data.core.state,
                        &mut device.renderer,
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                        now,
                        dt,
                    );
                }
            }
        } else {
            data.core.last_now = now;

            for (_node, device) in data.backend.devices.iter_mut() {
                let gpu_vendor_id = device.gpu_vendor_id;
                let exclusive_hdr_output = device.exclusive_hdr_output.clone();
                let all_outputs_stable = device.surfaces.values().all(|surface| {
                    surface.stable_vblank_count >= HDR_MIN_STABLE_VBLANKS
                        && surface.frame_queued_at.is_none()
                });
                for (_crtc, surface) in device.surfaces.iter_mut() {
                    if surface.frame_queued_at.is_some() {
                        // Wait for the matching vblank before preparing another buffer for this
                        // CRTC. Other outputs remain independent and continue rendering.
                        continue;
                    }

                    let owns_cursor = data.core.state.output_owns_cursor(surface.output_id);
                    let pending_damage =
                        data.core.state.output_has_pending_damage(surface.output_id);
                    let wants_screenshot = screenshot_output == Some(surface.output_id);
                    let portal_output_pending = data
                        .core
                        .state
                        .pending_portal_captures
                        .iter()
                        .any(|cap| cap.output_id == surface.output_id);
                    let should_skip = !data.core.state.render.redraw_all
                        && !data.core.state.screenshot_all_requested
                        && !portal_needs_composite
                        && !portal_output_pending
                        && !pending_damage
                        && !wants_screenshot
                        && !owns_cursor;

                    if should_skip {
                        continue;
                    }

                    let surface_cursor = data.core.state.render.sw_cursor_surface.is_some();
                    if owns_cursor && !surface_cursor {
                        // Retry KMS cursor each frame; SW overlay covers Skipped uploads.
                        data.core.state.drm_submit_hw_cursor = true;
                    }
                    data.core.state.drm_try_pass_cursor_this_frame = owns_cursor
                        && data.core.state.drm_submit_hw_cursor
                        && data.core.state.cursor_manager.visible()
                        && !surface_cursor;

                    let buffer_size = Size::from((surface.size.w, surface.size.h));

                    let any_hdr_requested = data.core.state.outputs.values().any(|output| {
                        output.hdr_requested
                            && output.hdr_supported
                            && crate::core::color::hdr_output_selected(&output.handle.name())
                    });
                    let hdr_target = hdr_runtime_apply_enabled(any_hdr_requested)
                        && data
                            .core
                            .state
                            .outputs
                            .get(&surface.output_id)
                            .map(|output| {
                                output.hdr_requested
                                    && surface.hdr_render_supported
                                    && crate::core::color::hdr_output_selected(
                                        &output.handle.name(),
                                    )
                            })
                            .unwrap_or(false);
                    if surface.hdr_transition_target.is_none()
                        && hdr_target != surface.hdr_enabled_applied
                    {
                        if hdr_target && !all_outputs_stable {
                            // Keep producing baseline SDR frames until this
                            // device's complete output topology has
                            // demonstrated a healthy event path. In particular,
                            // do not stack HDR connector changes directly onto
                            // the initial modesets while another CRTC is still
                            // waiting for its first successful presentations.
                            data.core.state.mark_redraw();
                        } else if hdr_target
                            && nvidia_kms_hdr_blocked(
                                gpu_vendor_id,
                                exclusive_hdr_output.as_deref()
                                    == Some(surface.output.name().as_str()),
                            )
                        {
                            if !surface.hdr_dual_block_logged {
                                surface.hdr_dual_block_logged = true;
                                flog_warn!(
                                    "HDR KMS blocked on {}: NVIDIA requires FOCALDESK_HDR_ALLOW_NVIDIA=1 and non-exclusive topologies also require FOCALDESK_HDR_NVIDIA_DUAL=1",
                                    surface.output.name()
                                );
                            }
                        } else if hdr_output::stage_hdr_output_state(
                            surface,
                            device.drm_output_manager.device(),
                            hdr_target,
                        ) {
                            surface.hdr_transition_target = Some(hdr_target);
                            surface.hdr_initial_modeset_pending = false;
                            surface.hdr_commit_deadline = Some(now + HDR_FRAME_TIMEOUT);
                            if let Some(output) =
                                data.core.state.outputs.get_mut(&surface.output_id)
                            {
                                output.hdr_transition_target = Some(hdr_target);
                            }
                            flog_warn!(
                                "HDR KMS transition staged on {}: target={hdr_target}",
                                surface.output.name()
                            );
                        } else if let Some(output) =
                            data.core.state.outputs.get_mut(&surface.output_id)
                        {
                            output.hdr_transition_target = None;
                            if exclusive_hdr_output.as_deref()
                                == Some(surface.output.name().as_str())
                            {
                                persist_exclusive_hdr_phase(
                                    ExclusiveHdrPhase::Failed,
                                    Some(surface.output.name().as_str()),
                                    Some("HDR connector state could not be staged"),
                                );
                                surface.hdr_commit_deadline = Some(now);
                            }
                        }
                    }

                    if let Some(output) = data.core.state.outputs.get_mut(&surface.output_id) {
                        output.hdr_kms_applied = surface.hdr_enabled_applied;
                        let render_active = crate::core::color::output_hdr_render_active(
                            output.hdr_requested,
                            output.hdr_supported,
                            output.hdr_kms_applied,
                        );
                        let exclusive_output =
                            exclusive_hdr_output.as_deref() == Some(surface.output.name().as_str());
                        let verification_pending = render_active
                            && exclusive_output
                            && surface.hdr_verify_started_at.is_some();
                        output.hdr_verification_pending = verification_pending;
                        output.hdr_enabled = hdr_active_status_verified(
                            render_active,
                            exclusive_output,
                            !verification_pending,
                        );
                    }

                    if portal_needs_composite {
                        data.core.state.render.redraw_all = true;
                    }

                    let mut prepared = prepare_output(
                        &mut data.core.state,
                        &mut device.renderer,
                        surface.output_id,
                        buffer_size,
                        &mut data.core.ui_state,
                        now,
                        dt,
                        portal_needs_composite,
                    )?;

                    let client_to_scene = data
                        .core
                        .state
                        .render
                        .chrome_shaders
                        .client_to_scene_linear
                        .clone();
                    let srgb_to_linear =
                        data.core.state.render.chrome_shaders.srgb_to_linear.clone();
                    let use_linear_sdr = use_linear_sdr_path(
                        &mut device.renderer,
                        &surface.render_targets,
                        surface.size,
                    ) && client_to_scene.is_some()
                        && srgb_to_linear.is_some();

                    if use_linear_sdr {
                        if let Err(err) = surface
                            .render_targets
                            .ensure_linear_offscreen(&mut device.renderer, surface.size)
                        {
                            flog(&format!(
                                "Linear SDR disabled on {} after FP16 allocation failed: {err}",
                                surface.output.name()
                            ));
                        }
                    }

                    let sync =
                        if use_linear_sdr && surface.render_targets.linear_offscreen.is_some() {
                            run_linear_staged_pass(
                                &mut data.core.state,
                                &mut device.renderer,
                                &mut surface.render_targets,
                                surface.output_id,
                                surface.size,
                                &mut prepared,
                                &mut data.core.ui_state,
                                &data.core.scene,
                                &data.core.output_state,
                                client_to_scene.as_ref().unwrap(),
                                srgb_to_linear.as_ref().unwrap(),
                            )?
                        } else {
                            run_sdr_pass(
                                &mut data.core.state,
                                &mut device.renderer,
                                &mut surface.render_targets,
                                surface.output_id,
                                surface.size,
                                &prepared,
                                &mut data.core.ui_state,
                                &data.core.scene,
                                &data.core.output_state,
                            )?
                        };
                    if surface.hdr_transition_target == Some(true)
                        && !surface.render_targets.encoded_hdr
                    {
                        flog_warn!(
                            "HDR PQ first-frame encode failed on {}; cancelling the staged KMS transition",
                            surface.output.name()
                        );
                        match hdr_detection::hdr_kms::configure_smithay_hdr_state(
                            surface,
                            device.drm_output_manager.device(),
                            false,
                            None,
                        ) {
                            Ok(true) => {
                                surface.hdr_transition_target = None;
                                surface.hdr_initial_modeset_pending = false;
                                surface.hdr_render_supported = false;
                                if exclusive_hdr_output.as_deref()
                                    == Some(surface.output.name().as_str())
                                {
                                    persist_exclusive_hdr_phase(
                                        ExclusiveHdrPhase::Failed,
                                        Some(surface.output.name().as_str()),
                                        Some("PQ first-frame encoding failed"),
                                    );
                                    surface.hdr_commit_deadline = Some(now);
                                } else {
                                    surface.hdr_commit_deadline = None;
                                }
                                if let Some(output) =
                                    data.core.state.outputs.get_mut(&surface.output_id)
                                {
                                    output.hdr_transition_target = None;
                                }
                            }
                            Ok(false) => {
                                flog_warn!(
                                    "Could not cancel staged HDR connector state on {}; withholding SDR frame for watchdog recovery",
                                    surface.output.name()
                                );
                                data.core.state.mark_redraw();
                                continue;
                            }
                            Err(err) => {
                                flog_warn!(
                                    "Failed to cancel staged HDR connector state on {}: {err}; withholding SDR frame for watchdog recovery",
                                    surface.output.name()
                                );
                                data.core.state.mark_redraw();
                                continue;
                            }
                        }
                    }
                    let exclusive_hdr_frame = surface.hdr_enabled_applied
                        && exclusive_hdr_output.as_deref() == Some(surface.output.name().as_str());
                    if exclusive_hdr_frame && !surface.render_targets.encoded_hdr {
                        flog_warn!(
                            "Exclusive HDR PQ encoding stopped during verification on {}; staging SDR rollback without clearing the ordinary HDR request",
                            surface.output.name()
                        );
                        persist_exclusive_hdr_phase(
                            ExclusiveHdrPhase::Failed,
                            Some(surface.output.name().as_str()),
                            Some("PQ encoding failed during HDR verification"),
                        );
                        if let Some(output) = data.core.state.outputs.get_mut(&surface.output_id) {
                            clear_runtime_hdr_request(output);
                        }
                        data.core.state.mark_redraw();
                        continue;
                    }
                    // `run_linear_staged_pass` finishes several dependent FBO
                    // submissions and returns the fence for the final encode.
                    // The texture is sampled again below by `render_frame`.
                    // Waiting only for capture consumers leaves that hand-off
                    // racy on the direct NVIDIA path: early base pixels are
                    // visible, while later client/egui regions can present as a
                    // solid blue rectangle. Screenshots did not contain the
                    // artifact because their readback happened to wait first.
                    //
                    // Smithay does not receive this producer fence through the
                    // TextureRenderElement, so make the dependency explicit
                    // before either capture or scanout samples the texture.
                    device.renderer.wait(&sync)?;

                    if let Some((texture, encoding)) = capture_source_texture(surface) {
                        crate::core::portal::publish_portal_capture_source(
                            &mut data.core.state,
                            surface.output_id,
                            texture,
                            surface.size,
                            encoding,
                            now,
                        );
                    }

                    if portal_output_pending {
                        crate::core::portal::complete_pending_portal_captures_for_output(
                            &mut data.core.state,
                            &mut device.renderer,
                            &mut data.core.ui_state,
                            &data.core.scene,
                            &data.core.output_state,
                            surface.output_id,
                            now,
                            dt,
                        );
                    }

                    if wants_screenshot {
                        data.core.state.screenshot_seq += 1;
                        let seq = data.core.state.screenshot_seq;
                        let output_name = format!("output-{}", surface.output_id.0);

                        match save_offscreen_screenshot(
                            &mut device.renderer,
                            surface,
                            &output_name,
                            seq,
                        ) {
                            Ok(path) => {
                                flog(&format!("Screenshot saved to {}", path.display()));
                            }
                            Err(err) => {
                                flog(&format!("Screenshot failed: {err}"));
                            }
                        }
                        data.core.state.clear_screenshot_request(surface.output_id);
                    }

                    // now borrow immutably after the mutable borrow is gone
                    let texture = present_source_texture(surface)
                        .expect("offscreen texture missing")
                        .clone();
                    let output_scale = data
                        .core
                        .state
                        .outputs
                        .get(&surface.output_id)
                        .map(|o| o.scale_factor)
                        .unwrap_or(1.0)
                        .max(1.0);
                    let present_logical_size = Size::<i32, Logical>::from((
                        (surface.size.w as f64 / output_scale).round() as i32,
                        (surface.size.h as f64 / output_scale).round() as i32,
                    ));
                    let present_src = Rectangle::<f64, Logical>::from_loc_and_size(
                        (0.0, 0.0),
                        (surface.size.w as f64, surface.size.h as f64),
                    );
                    let force_hdr_verification_present =
                        exclusive_hdr_frame && surface.hdr_verify_started_at.is_some();
                    let present_frame_damage: Vec<Rectangle<i32, Buffer>> =
                        if force_hdr_verification_present {
                            // The FP16/PQ targets are retained textures. Resetting the
                            // DamageBag every frame made its commit counter repeatedly
                            // return to 1, so Smithay compared commit 1 with commit 1,
                            // classified the frame as empty, and never queued another
                            // page flip for the HDR verifier to count. Keep the damage
                            // history monotonic and explicitly damage the full scanout
                            // while verification needs a real presentation.
                            vec![Rectangle::from_loc_and_size(
                                (0, 0),
                                (surface.size.w, surface.size.h),
                            )]
                        } else {
                            prepared
                                .frame_ctx
                                .damage
                                .iter()
                                .map(|rect| {
                                    Rectangle::<i32, Buffer>::from_loc_and_size(
                                        (rect.loc.x, rect.loc.y),
                                        (rect.size.w, rect.size.h),
                                    )
                                })
                                .collect()
                        };
                    surface.present_damage.add(present_frame_damage);
                    let present_damage = surface.present_damage.snapshot();

                    let texture_elem = TextureRenderElement::from_texture_with_damage(
                        surface.present_render_id.clone(),
                        device.renderer.context_id(),
                        (0.0, 0.0),
                        texture,
                        1,
                        Transform::Normal,
                        Some(1.0),
                        Some(present_src),
                        Some(present_logical_size),
                        None,
                        present_damage,
                        Kind::Unspecified,
                    );

                    let mut present_elements: Vec<DrmPresentElement> =
                        Vec::with_capacity(if data.core.state.drm_try_pass_cursor_this_frame {
                            2
                        } else {
                            1
                        });

                    if data.core.state.drm_try_pass_cursor_this_frame {
                        if let Some(ct) = data.core.state.render.sw_cursor_texture.as_ref() {
                            let scale = data
                                .core
                                .state
                                .outputs
                                .get(&surface.output_id)
                                .map(|o| o.scale)
                                .unwrap_or_else(|| Scale::from((1.0, 1.0)));
                            let rel = data
                                .core
                                .state
                                .pointer_relative_to_output_logical(surface.output_id)
                                .unwrap_or(data.core.state.pointer_pos);
                            let phys: Point<i32, Physical> =
                                rel.to_physical_precise_round::<f64, i32>(scale);
                            let (hx, hy) = data.core.state.render.sw_cursor_hotspot;
                            let (tw, th) = data.core.state.render.sw_cursor_tex_size;
                            let cursor_logical_size = Size::<i32, Logical>::from((
                                (tw as f64 / output_scale).round().max(1.0) as i32,
                                (th as f64 / output_scale).round().max(1.0) as i32,
                            ));
                            let cursor_src = Rectangle::<f64, Logical>::from_loc_and_size(
                                (0.0, 0.0),
                                (tw as f64, th as f64),
                            );
                            let cursor_elem = TextureRenderElement::from_static_texture(
                                data.core.state.drm_cursor_render_id.clone(),
                                device.renderer.context_id(),
                                Point::<f64, Physical>::from((
                                    (phys.x - hx) as f64,
                                    (phys.y - hy) as f64,
                                )),
                                ct.clone(),
                                1,
                                Transform::Normal,
                                Some(1.0),
                                Some(cursor_src),
                                Some(cursor_logical_size),
                                None,
                                Kind::Cursor,
                            );
                            present_elements.push(DrmPresentElement::Texture(cursor_elem));
                        }
                    }

                    present_elements.push(DrmPresentElement::Texture(texture_elem));

                    // A page-flip/commit failure here (e.g. a transient
                    // permission-denied race while DRM master is being handed
                    // back after resume-from-suspend) must not bring down the
                    // whole compositor: that kills the session and dumps the
                    // user back to the GDM login screen. Skip this output for
                    // one frame and let the next tick retry instead.
                    let frame_result = match surface.drm_output.render_frame(
                        &mut device.renderer,
                        &present_elements,
                        Color32F::new(0.0, 0.0, 0.0, 1.0),
                        FrameFlags::DEFAULT,
                    ) {
                        Ok(frame_result) => frame_result,
                        Err(err) => {
                            flog(&format!(
                                "DRM render_frame failed for output {:?}: {err}",
                                surface.output_id
                            ));
                            data.core.state.mark_redraw();
                            continue;
                        }
                    };

                    data.core.state.update_cursor_policy_after_drm_present(
                        &frame_result.states,
                        frame_result.cursor_element.is_some(),
                    );

                    if !frame_result.is_empty {
                        if let Err(err) = surface.drm_output.queue_frame(None) {
                            flog(&format!(
                                "DRM queue_frame failed for output {:?}: {err}",
                                surface.output_id
                            ));
                            data.core.state.mark_redraw();
                            continue;
                        }
                        surface.frame_queued_at = Some(now);
                        data.core.state.compositor_ready = true;
                    }
                }

                // screen capture all outputs
                if data.core.state.screenshot_all_requested {
                    data.core.state.screenshot_seq += 1;
                    let seq = data.core.state.screenshot_seq;

                    match save_all_outputs_screenshot(
                        &mut device.renderer,
                        &mut device.surfaces,
                        seq,
                    ) {
                        Ok(path) => flog(&format!(
                            "All-outputs screenshot saved to {}",
                            path.display()
                        )),
                        Err(err) => flog(&format!("All-outputs screenshot failed: {err}")),
                    }
                }
            }

            if portal_pending {
                if let Some(device) = data.backend.devices.values_mut().next() {
                    crate::core::portal::complete_pending_portal_captures(
                        &mut data.core.state,
                        &mut device.renderer,
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                        now,
                        dt,
                    );
                }
            }
        }

        data.core.state.screenshot_all_requested = false;

        data.core.state.clear_repaint_request();
        data.core.state.render.frame_no += 1;

        let frame_time_ms = data.core.start.elapsed().as_millis() as u32;
        data.core.state.send_frame_callbacks(frame_time_ms);
    }

    // The main loop above only exits while the machine stays up via Logout
    // (`running = false`); Suspend/Hibernate keep the compositor alive across
    // resume, and Restart/Shutdown take the whole machine down via powerd, so
    // there's nothing to clean up in those cases. Stop the session target so
    // the per-domain helper daemons (`WantedBy=focaldesk-session.target`) are
    // torn down cleanly rather than left running orphaned until next login.
    let exclusive_state = load_exclusive_hdr_state();
    if exclusive_state.session_id == Some(std::process::id()) {
        match exclusive_state.phase {
            ExclusiveHdrPhase::Active => persist_exclusive_hdr_phase(
                ExclusiveHdrPhase::Requested,
                exclusive_state.connector.as_deref(),
                None,
            ),
            ExclusiveHdrPhase::Starting | ExclusiveHdrPhase::Verifying => {
                persist_exclusive_hdr_phase(
                    ExclusiveHdrPhase::Failed,
                    exclusive_state.connector.as_deref(),
                    Some("session exited before exclusive HDR verification completed"),
                );
            }
            _ => {}
        }
    }
    stop_focaldesk_session_target();

    Ok(())
}

fn connector_name(info: &drm::control::connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

// ext-image-copy-capture has no color metadata. Advertise only RGB formats
// that follow FocalDesk's explicit sRGB/Rec.709 portal contract, preferring
// 10-bit targets so a capable PipeWire/OBS path does not quantize the FP16
// compositor scene to 8-bit unnecessarily.
const PORTAL_CAPTURE_FORMAT_PREFERENCE: [Fourcc; 8] = [
    Fourcc::Xrgb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr2101010,
    Fourcc::Xrgb8888,
    Fourcc::Xbgr8888,
    Fourcc::Argb8888,
    Fourcc::Abgr8888,
];

fn portal_capture_format_priority(format: Fourcc) -> Option<usize> {
    PORTAL_CAPTURE_FORMAT_PREFERENCE
        .iter()
        .position(|candidate| *candidate == format)
}

fn portal_capture_format_allowed(format: Fourcc, require_ten_bit: bool) -> bool {
    portal_capture_format_priority(format).is_some_and(|priority| !require_ten_bit || priority < 4)
}

fn dmabuf_capture_formats(format_set: &FormatSet) -> Vec<(Fourcc, Vec<Modifier>)> {
    let require_ten_bit = crate::core::portal::portal_capture_color_mode().requires_ten_bit();
    let mut formats: Vec<(Fourcc, Vec<Modifier>)> = Vec::new();
    for format in format_set.iter() {
        if !portal_capture_format_allowed(format.code, require_ten_bit) {
            continue;
        }
        if let Some((_, modifiers)) = formats.iter_mut().find(|(code, _)| *code == format.code) {
            if !modifiers.contains(&format.modifier) {
                modifiers.push(format.modifier);
            }
        } else {
            formats.push((format.code, vec![format.modifier]));
        }
    }
    formats
        .sort_by_key(|(format, _)| portal_capture_format_priority(*format).unwrap_or(usize::MAX));
    formats
}

#[cfg(test)]
mod portal_capture_format_tests {
    use super::*;

    #[test]
    fn ten_bit_capture_formats_are_preferred_to_eight_bit() {
        assert!(
            portal_capture_format_priority(Fourcc::Xbgr2101010)
                < portal_capture_format_priority(Fourcc::Xbgr8888)
        );
        assert!(
            portal_capture_format_priority(Fourcc::Argb2101010)
                < portal_capture_format_priority(Fourcc::Argb8888)
        );
    }

    #[test]
    fn untagged_capture_rejects_formats_without_the_rgb_contract() {
        assert_eq!(portal_capture_format_priority(Fourcc::Nv12), None);
    }

    #[test]
    fn wide_gamut_capture_rejects_eight_bit_and_yuv_formats() {
        assert!(portal_capture_format_allowed(Fourcc::Xbgr2101010, true));
        assert!(!portal_capture_format_allowed(Fourcc::Xbgr8888, true));
        assert!(!portal_capture_format_allowed(Fourcc::Nv12, true));
    }
}

pub fn make_drm_gpu(
    node: DrmNode,
    fd: DrmDeviceFd,
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    registration_token: calloop::RegistrationToken,
) -> Result<DrmGpu> {
    let render_node = {
        let display = unsafe { EGLDisplay::new(gbm.clone())? };
        let egl_device = EGLDevice::device_for_display(&display)?;
        let _context = EGLContext::new(&display)?;
        if egl_device.is_software() {
            None
        } else {
            egl_device
                .try_get_render_node()
                .ok()
                .flatten()
                .or(Some(node))
        }
    };

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );

    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), render_node.into());

    let color_formats = [
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
        Fourcc::Abgr2101010,
        Fourcc::Xbgr2101010,
        Fourcc::Argb2101010,
        Fourcc::Xrgb2101010,
    ];

    let render_formats = FormatSet::default();

    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator.clone(),
        framebuffer_exporter.clone(),
        Some(gbm.clone()),
        color_formats.into_iter(),
        render_formats,
    );

    Ok(DrmGpu {
        node,
        fd,
        gbm,
        registration_token,
        render_node,
        allocator,
        framebuffer_exporter,
        drm_output_manager,
        outputs: HashMap::new(),
    })
}

fn device_added(
    data: &mut DrmLoopData,
    loop_handle: &LoopHandle<'_, DrmLoopData>,
    node: DrmNode,
    path: &Path,
) -> Result<()> {
    flog(&format!(
        "DRM device added: {:?} ({})",
        node,
        path.display()
    ));
    flog(&format!(
        "About to open device-added DRM node {}",
        path.display()
    ));
    // Open device through the active seat
    let fd = match data
        .backend
        .session
        .open(path, OFlags::RDWR | OFlags::CLOEXEC)
    {
        Ok(fd) => fd,
        Err(err) => {
            flog(&format!(
                "Failed to open DRM device {} via libseat/session: {:?}",
                path.display(),
                err
            ));
            return Err(anyhow!(
                "Failed to open DRM device {}: {:?}",
                path.display(),
                err
            ));
        }
    };

    flog("Device-added DRM node open succeeded");

    let gpu_vendor_id = drm_card_vendor_id(path);

    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, notifier) =
        DrmDevice::new(fd.clone(), true).context("Failed to create DrmDevice for added node")?;
    let gbm = GbmDevice::new(fd.clone()).context("Failed to create GBM device for node")?;

    let egl_display = unsafe { egl::EGLDisplay::new(gbm.clone()) }
        .context("Failed to create EGLDisplay for DRM node")?;
    let egl_device =
        EGLDevice::device_for_display(&egl_display).context("Failed to query EGLDevice")?;

    if egl_device.is_software() {
        flog(
            "EGL reports a software rasterizer (e.g. llvmpipe). Check drivers if you expected GPU acceleration.",
        );
    }

    // Prefer the render node from the EGL driver so scan-out import matches where GL allocates.
    let render_node_for_gpu = if egl_device.is_software() {
        None
    } else {
        egl_device
            .try_get_render_node()
            .ok()
            .flatten()
            .or(Some(node))
    };

    // Prefer GLES 3 for core synchronization and framebuffer capabilities.  Keep
    // the former configless GLES 2 context as a compatibility fallback: asking
    // Smithay for an explicit GL version also requires selecting an EGLConfig,
    // which some older or unusual DRM drivers may reject.
    let preferred_context = EGLContext::new_with_config_and_priority(
        &egl_display,
        GlAttributes {
            version: (3, 0),
            profile: None,
            debug: cfg!(debug_assertions),
            vsync: false,
        },
        PixelFormatRequirements::_8_bit(),
        ContextPriority::High,
    );

    let mut renderer = match preferred_context {
        Ok(context) => match unsafe { GlesRenderer::new(context) } {
            Ok(renderer) => {
                flog("Created preferred OpenGL ES 3.0 context for DRM renderer");
                renderer
            }
            Err(err) => {
                flog(&format!(
                    "OpenGL ES 3.0 DRM renderer initialization failed ({err:?}); falling back to OpenGL ES 2.0"
                ));
                let context = EGLContext::new_with_priority(&egl_display, ContextPriority::High)
                    .context("Failed to create fallback OpenGL ES 2.0 context for DRM node")?;
                unsafe { GlesRenderer::new(context) }
                    .context("Failed to create fallback OpenGL ES 2.0 renderer for DRM node")?
            }
        },
        Err(err) => {
            flog(&format!(
                "OpenGL ES 3.0 DRM context unavailable ({err:?}); falling back to OpenGL ES 2.0"
            ));
            let context = EGLContext::new_with_priority(&egl_display, ContextPriority::High)
                .context("Failed to create fallback OpenGL ES 2.0 context for DRM node")?;
            unsafe { GlesRenderer::new(context) }
                .context("Failed to create fallback OpenGL ES 2.0 renderer for DRM node")?
        }
    };

    match renderer.bind_wl_display(&data.core.display.handle()) {
        Ok(_) => flog("EGL Wayland display bound for DRM renderer"),
        Err(err) => flog(&format!(
            "Failed to bind EGL Wayland display for DRM renderer: {err:?}"
        )),
    }

    if data.core.state.dmabuf_global.is_none() {
        let dmabuf_node = render_node_for_gpu.unwrap_or(node);
        let dmabuf_formats = renderer.dmabuf_formats();
        let hdr_client_formats = dmabuf_formats
            .iter()
            .copied()
            .filter(|format| {
                matches!(
                    format.code,
                    Fourcc::Abgr16161616f
                        | Fourcc::Argb16161616f
                        | Fourcc::Abgr2101010
                        | Fourcc::Argb2101010
                        | Fourcc::Xbgr2101010
                        | Fourcc::Xrgb2101010
                )
            })
            .collect::<Vec<_>>();
        let mut feedback_builder =
            DmabufFeedbackBuilder::new(dmabuf_node.dev_id(), dmabuf_formats.iter().copied());
        if !hdr_client_formats.is_empty() {
            use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
            feedback_builder = feedback_builder.add_preference_tranche(
                dmabuf_node.dev_id(),
                Some(TrancheFlags::Scanout),
                hdr_client_formats.iter().copied(),
            );
            flog(format!(
                "linux-dmabuf prefers HDR client formats: {:?}",
                hdr_client_formats
                    .iter()
                    .map(|format| format.code)
                    .collect::<Vec<_>>()
            ));
        }
        let default_feedback = feedback_builder
            .build()
            .context("Failed to build linux-dmabuf feedback")?;
        let global = data
            .core
            .state
            .dmabuf_state
            .create_global_with_default_feedback::<DesktopState>(
                &data.core.display.handle(),
                &default_feedback,
            );

        data.core.state.dmabuf_global = Some(global);
        data.core.state.dmabuf_node = Some(dmabuf_node);
        flog(&format!(
            "linux-dmabuf enabled on DRM node {:?}",
            dmabuf_node
        ));
    }

    let render_format_set = renderer.egl_context().dmabuf_render_formats().clone();
    data.core.state.portal_dmabuf_formats = dmabuf_capture_formats(&render_format_set);
    let portal_color_mode = crate::core::portal::portal_capture_color_mode();
    flog(format!(
        "portal capture color contract: mode={portal_color_mode:?} formats={:?}",
        data.core
            .state
            .portal_dmabuf_formats
            .iter()
            .map(|(format, _)| *format)
            .collect::<Vec<_>>()
    ));
    if portal_color_mode.requires_ten_bit() && data.core.state.portal_dmabuf_formats.is_empty() {
        flog(
            "BT.2020 SDR portal capture unavailable: renderer exposes no 10-bit RGB DMA-BUF format",
        );
    }
    let render_formats = render_format_set.iter().copied().collect::<Vec<_>>();

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), render_node_for_gpu.into());
    let mut drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        framebuffer_exporter,
        Some(gbm.clone()),
        DRM_SCANOUT_FORMAT_PREFERENCE,
        render_formats,
    );

    let mut surfaces = HashMap::new();

    // Scan connectors and attach anything connected
    let res = drm_output_manager
        .device()
        .resource_handles()
        .context("Failed to get DRM resources")?;

    let exclusive_selector =
        prepare_exclusive_hdr_attempt(crate::core::color::exclusive_hdr_output_selector());
    let mut exclusive_candidates = Vec::new();
    for conn in res.connectors() {
        let info = drm_output_manager
            .device()
            .get_connector(*conn, false)
            .context("Failed to inspect connector for exclusive HDR preflight")?;
        if info.state() != drm::control::connector::State::Connected {
            continue;
        }
        let name = connector_name(&info);
        let edid = connector_edid(drm_output_manager.device(), *conn);
        let support = hdr_detection::connector_hdr_support(
            drm_output_manager.device(),
            *conn,
            edid.as_deref(),
        );
        let has_crtc = info.encoders().iter().any(|encoder| {
            drm_output_manager
                .device()
                .get_encoder(*encoder)
                .ok()
                .is_some_and(|encoder| !res.filter_crtcs(encoder.possible_crtcs()).is_empty())
        });
        let capable = !info.modes().is_empty()
            && has_crtc
            && support.can_signal_hdr10()
            && support.bpc_control_allows_ten_bit()
            && hdr_driver_allows_output(gpu_vendor_id);
        exclusive_candidates.push((name, capable));
    }

    let mut exclusive_hdr_output = match select_exclusive_hdr_target(
        exclusive_selector.as_deref(),
        &exclusive_candidates,
    ) {
        Ok(target) => target,
        Err(reason) => {
            flog_warn!(
                "Exclusive HDR preflight failed: {reason}; keeping all connected outputs active in SDR"
            );
            persist_exclusive_hdr_phase(
                ExclusiveHdrPhase::Failed,
                exclusive_selector.as_deref(),
                Some(&reason),
            );
            None
        }
    };
    if let Some(target) = exclusive_hdr_output.as_deref() {
        flog_warn!(
            "Exclusive HDR preflight accepted connector {target}; other outputs will remain disabled for this session"
        );
    }

    let mut connector_handles = res.connectors().to_vec();
    connector_handles.sort_by_key(|conn| {
        let name = drm_output_manager
            .device()
            .get_connector(*conn, false)
            .ok()
            .map(|info| connector_name(&info));
        exclusive_hdr_output
            .as_ref()
            .is_none_or(|selected| name.as_ref() != Some(selected))
    });

    let mut used_crtcs = std::collections::HashSet::new();
    let mut initialized_one = false;

    let mut id: u64 = 1;

    let mut next_x = 0;
    let configured_displays = load_display_config();
    let any_capable_hdr_requested = exclusive_candidates.iter().any(|(name, capable)| {
        *capable && configured_display_hdr_requested(&configured_displays, name)
    });

    for conn in &connector_handles {
        let info = drm_output_manager
            .device()
            .get_connector(*conn, false)
            .context("Failed to get connector info")?;

        if info.state() != drm::control::connector::State::Connected {
            continue;
        }

        //flog(&format!("Found connected connector: {:?}", conn));
        let name = connector_name(&info);

        if exclusive_hdr_output
            .as_deref()
            .is_some_and(|selected| selected != name)
        {
            flog(&format!(
                "Exclusive HDR: leaving connected output {name} disabled"
            ));
            continue;
        }

        let connector_size = info.size();
        flog(&format!(
            "CONNECTOR: name={} handle={:?} state={:?} size_mm={}x{} encoders={:?} modes={}",
            name,
            info.handle(),
            info.state(),
            connector_size.map(|size| size.0).unwrap_or(0),
            connector_size.map(|size| size.1).unwrap_or(0),
            info.encoders(),
            info.modes().len(),
        ));

        for (i, m) in info.modes().iter().enumerate() {
            flog(&format!(
                "  mode[{}]: {}x{} @ {}Hz type={:?}",
                i,
                m.size().0,
                m.size().1,
                m.vrefresh(),
                m.mode_type(),
            ));
        }

        let output_name = name.clone();
        let edid = connector_edid(drm_output_manager.device(), *conn);
        let hdr_support = hdr_detection::connector_hdr_support(
            drm_output_manager.device(),
            *conn,
            edid.as_deref(),
        );
        hdr_detection::log_hdr_support(&output_name, &hdr_support);
        let hdr_requested_from_config =
            configured_display_hdr_requested(&configured_displays, &output_name);
        let hdr_requested_config = exclusive_hdr_output.as_deref() == Some(output_name.as_str())
            || crate::core::color::matching_hdr_request(
                nvidia_dual_head_hdr_allowed(),
                exclusive_hdr_output.is_some(),
                crate::core::color::hdr_output_selector_active(),
                hdr_support.can_signal_hdr10() && hdr_support.bpc_control_allows_ten_bit(),
                hdr_requested_from_config,
                any_capable_hdr_requested,
            );
        if hdr_requested_config && !hdr_requested_from_config {
            flog_warn!(
                "HDR10 matching: requesting {output_name} so sibling HDR-capable outputs share the same PQ encode"
            );
            enable_persisted_hdr_request(&output_name);
        }
        let hdr_safe_mode_requested = hdr_requested_config
            && hdr_support.can_signal_hdr10()
            && hdr_support.bpc_control_allows_ten_bit();
        let mode = select_connector_mode(info.modes());

        if let Some(mode) = mode {
            let (w, h) = mode.size();

            if hdr_safe_mode_requested {
                flog_warn!(
                    "HDR safe mode selected: output={output_name} mode={w}x{h}@{}Hz ceiling={}Hz",
                    mode.vrefresh(),
                    OUTPUT_MAX_REFRESH_HZ,
                );
            } else {
                flog(&format!("Selected mode: {}x{} @ {}", w, h, mode.vrefresh()));
            }

            let fallback_mm =
                physical_size_mm_from_pixels(Size::<i32, Physical>::from((w as i32, h as i32)));
            let (mm_w, mm_h) = info
                .size()
                .filter(|(mm_w, mm_h)| *mm_w > 0 && *mm_h > 0)
                .map(|(mm_w, mm_h)| (mm_w as i32, mm_h as i32))
                .unwrap_or(fallback_mm);

            let edid_identity = edid.as_deref().and_then(parse_edid_identity);
            let make = edid_identity
                .as_ref()
                .map(|identity| identity.make.clone())
                .unwrap_or_else(|| "FocalDesk".to_string());
            let model = edid_identity
                .as_ref()
                .map(|identity| identity.model.clone())
                .unwrap_or_else(|| info.interface().as_str().to_string());
            let serial_number = edid_identity
                .as_ref()
                .map(|identity| identity.serial_number.clone())
                .unwrap_or_else(|| {
                    format!("{}-{}", info.interface().as_str(), info.interface_id())
                });

            let output = Output::new(
                output_name.clone(),
                PhysicalProperties {
                    size: (mm_w, mm_h).into(),
                    subpixel: Subpixel::Unknown,
                    make: make.clone(),
                    model: model.clone(),
                    serial_number: serial_number.clone(),
                },
            );

            // Logical layout in global compositor space (must match `register_output_entry` / `map_output`).
            // wl_output + xdg_output advertise this to clients; leaving (0,0) stacks every head at the origin
            // (e.g. OBS projector shows all DRM outputs on top of each other).
            let output_scale = configured_display_scale(&configured_displays, &output_name);
            let output_scale_int = output_scale.round().max(1.0) as i32;
            let logical_size = Size::<i32, Logical>::from((
                (w as f64 / output_scale).round() as i32,
                (h as f64 / output_scale).round() as i32,
            ));
            let saved_display = configured_displays
                .iter()
                .find(|display| display.name == output_name);
            let origin = saved_display
                .map(|display| Point::<i32, Logical>::from((display.logical_x, display.logical_y)))
                .unwrap_or_else(|| Point::<i32, Logical>::from((next_x, 0)));
            let chrome_layout = build_chrome_layout(
                logical_size,
                data.core.state.chrome.metrics.topbar_h,
                data.core.state.chrome.metrics.sidebar_w,
            );

            let refresh_mhz = ((mode.vrefresh() as i32).max(60)) * 1000;
            let wl_mode = WlMode {
                size: (w as i32, h as i32).into(),
                refresh: refresh_mhz,
            };

            output.change_current_state(
                Some(wl_mode),
                Some(Transform::Normal),
                Some(smithay::output::Scale::Custom {
                    advertised_integer: output_scale_int,
                    fractional: output_scale,
                }),
                Some(origin),
            );
            output.set_preferred(wl_mode);
            output.create_global::<DesktopState>(&data.core.display.handle());

            flog(&format!(
                "Wayland output advertised: name={} px={}x{} logical={}x{} scale={} mm={}x{} refresh_mhz={} make={:?} model={:?} serial={:?}",
                output_name,
                w,
                h,
                logical_size.w,
                logical_size.h,
                output_scale,
                mm_w,
                mm_h,
                wl_mode.refresh,
                make,
                model,
                serial_number,
            ));
            flog(&format!(
                "DRM chrome layout: name={} topbar={:?} status_wells={:?} clock={:?}",
                output_name,
                chrome_layout.topbar.outer,
                chrome_layout.topbar.status_wells,
                chrome_layout.topbar.clock_well,
            ));

            let crtc = info.encoders().iter().find_map(|enc| {
                let enc_info = drm_output_manager.device().get_encoder(*enc).ok()?;

                res.filter_crtcs(enc_info.possible_crtcs())
                    .into_iter()
                    .find(|candidate| !used_crtcs.contains(candidate))
            });

            let Some(crtc) = crtc else {
                flog(&format!("No CRTC available for connector {:?}", conn));
                continue;
            };

            used_crtcs.insert(crtc);

            let planes = drm_output_manager
                .device()
                .planes(&crtc)
                .context("Failed to query planes for connector")?;

            let mut allocator = GbmAllocator::new(
                gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );

            let tex_phys_size: Size<i32, Physical> = Size::from((i32::from(w), i32::from(h)));

            let _gbm_buffer = allocator.create_buffer(
                tex_phys_size.w as u32,
                tex_phys_size.h as u32,
                Fourcc::Argb8888,
                &[DrmModifier::Linear],
            )?;
            let linear_sdr_supported = supports_linear_sdr(&mut renderer, tex_phys_size);
            flog(&format!(
                "Linear SDR probe: output={output_name} format={:?} supported={linear_sdr_supported}",
                crate::core::linear_compositing::LINEAR_SDR_FORMAT
            ));
            let hdr_format = select_hdr_offscreen_format(&mut renderer, tex_phys_size);
            let hdr_offscreen_supported = hdr_format.is_some();
            flog(&format!(
                "HDR offscreen probe: output={output_name} format={hdr_format:?} supported={hdr_offscreen_supported}"
            ));
            let drm_output = drm_output_manager
                .lock()
                .initialize_output::<_, SolidColorRenderElement>(
                    crtc,
                    mode,
                    &[*conn],
                    &output,
                    Some(planes),
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                )?;

            let (scanout_format, scanout_modifiers) =
                drm_output.with_compositor(|c| (c.format(), c.modifiers().to_vec()));
            let ten_bit_scanout_active = HDR_SCANOUT_FORMATS.contains(&scanout_format);
            if ten_bit_scanout_active {
                flog_warn!(
                    "10-bit scanout selected at output initialization: output={output_name} format={scanout_format:?} modifiers={scanout_modifiers:?}"
                );
            } else {
                flog_warn!(
                    "10-bit scanout unavailable at output initialization for {output_name}; retaining format={scanout_format:?} modifiers={scanout_modifiers:?}"
                );
            }
            let hdr_working_format =
                linear_sdr_supported.then_some(crate::core::linear_compositing::LINEAR_SDR_FORMAT);
            let hdr_render_supported = hdr_output_capable(
                &hdr_support,
                hdr_format,
                hdr_working_format,
                ten_bit_scanout_active,
                gpu_vendor_id,
            );
            if exclusive_hdr_output.as_deref() == Some(output_name.as_str())
                && !hdr_render_supported
            {
                flog_warn!(
                    "Exclusive HDR runtime probe failed on {output_name}; keeping all connected outputs active in SDR"
                );
                persist_exclusive_hdr_phase(
                    ExclusiveHdrPhase::Failed,
                    Some(&output_name),
                    Some("HDR renderer or 10-bit runtime probe failed"),
                );
                exclusive_hdr_output = None;
            }
            let driver_allowed = hdr_driver_allows_output(gpu_vendor_id);
            flog_warn!(
                "HDR capability: output={output_name} operational={hdr_render_supported} metadata_property={} bt2020_colorspace={} max_bpc={:?} current_max_bpc={:?} driver_managed_bpc={} edid_hdr={} edid_type1={} edid_pq={} edid_metadata={} fp16_working={} pq_target={hdr_format:?} ten_bit_scanout={ten_bit_scanout_active} scanout_format={scanout_format:?} driver_allowed={driver_allowed}",
                hdr_support.has_hdr_metadata_property,
                hdr_support.has_bt2020_colorspace,
                hdr_support.max_bpc,
                hdr_support.current_max_bpc,
                hdr_support.max_bpc.is_none(),
                hdr_support.edid_hdr_static_metadata,
                hdr_support.edid_static_metadata_type1,
                hdr_support.edid_pq,
                hdr_support.edid_hdr_metadata.is_some(),
                hdr_working_format.is_some(),
            );

            //let offscreen = OffscreenOutput {
            //    size: tex_phys_size,
            //    texture: None,
            //};

            let output_id = OutputId(id);
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                out.handle = output.clone();
                out.physical_size = Size::<i32, Physical>::from((w as i32, h as i32));
                out.logical_size = logical_size;
                out.scale_factor = output_scale;
                out.scale = smithay::utils::Scale::from((output_scale, output_scale));
            }

            data.core.state.register_output_entry(
                output_id,
                output.clone(),
                origin,
                Size::<i32, Physical>::from((w as i32, h as i32)),
                output_scale,
            );
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                sync_output_hdr_flags(out, &hdr_support, hdr_requested_config);
            }

            data.core.state.set_output_monitor_identity(
                output_id,
                make.clone(),
                model.clone(),
                serial_number.clone(),
                edid.clone(),
            );
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                out.color_profile_override =
                    configured_display_color_profile(&configured_displays, &output_name);
                out.icc_profile_path = configured_displays
                    .iter()
                    .find(|display| display.name == output_name)
                    .and_then(|display| display.icc_profile_path.clone());
            }
            data.core.state.refresh_output_color(output_id);

            if saved_display.is_some_and(|display| display.primary) || !initialized_one {
                data.core.state.primary_output = output_id;
            }

            let mut surface = DrmSurfaceState {
                connector: *conn,
                output,
                mode: wl_mode,
                size: Size::<i32, Physical>::from((w as i32, h as i32)),
                output_id,
                origin,
                present_render_id: Id::new(),
                present_damage: DamageBag::default(),
                render_targets: LinearOffscreenTargets {
                    linear_supported: linear_sdr_supported,
                    hdr_supported: hdr_offscreen_supported,
                    hdr_format,
                    ..LinearOffscreenTargets::default()
                },
                hdr_support,
                hdr_metadata_blob: None,
                hdr_enabled_applied: false,
                hdr_transition_target: None,
                hdr_initial_modeset_pending: false,
                hdr_render_supported,
                frame_queued_at: None,
                stable_vblank_count: 0,
                hdr_verify_vblank_count: 0,
                hdr_verify_started_at: None,
                hdr_commit_deadline: None,
                hdr_dual_block_logged: false,
                drm_output,
            };

            // Exclusive HDR is a startup topology, not a live toggle. Smithay
            // has created the pending CRTC/mode/connector state above, but no
            // real commit has occurred yet. Attach BT.2020 and the HDR10
            // metadata now so the first PQ framebuffer, primary-plane format,
            // mode, connector, and HDR properties are submitted atomically.
            // Ordinary HDR requests deliberately continue through the
            // baseline-SDR/live-transition path in the render loop.
            if exclusive_hdr_output.as_deref() == Some(output_name.as_str())
                && surface.hdr_render_supported
            {
                let initial_hdr = hdr_detection::hdr_kms::ensure_hdr_metadata_blob(
                    drm_output_manager.device(),
                    &surface.hdr_support,
                    &mut surface.hdr_metadata_blob,
                )
                .and_then(|blob| {
                    hdr_detection::hdr_kms::configure_smithay_hdr_state(
                        &surface,
                        drm_output_manager.device(),
                        true,
                        Some(blob),
                    )
                });

                match initial_hdr {
                    Ok(true) => {
                        surface.hdr_transition_target = Some(true);
                        surface.hdr_initial_modeset_pending = true;
                        surface.hdr_commit_deadline = Some(Instant::now() + HDR_FRAME_TIMEOUT);
                        if let Some(output) = data.core.state.outputs.get_mut(&output_id) {
                            output.hdr_transition_target = Some(true);
                        }
                        data.core.state.mark_redraw();
                        flog_warn!(
                            "Exclusive HDR initial modeset armed on {output_name}: first scanout will be PQ with BT.2020 and HDR10 metadata"
                        );
                    }
                    Ok(false) => {
                        let reason =
                            "HDR connector state could not be attached to the initial modeset";
                        flog_warn!(
                            "Exclusive HDR initial modeset failed on {output_name}: {reason}; restoring the ordinary SDR topology"
                        );
                        persist_exclusive_hdr_phase(
                            ExclusiveHdrPhase::Failed,
                            Some(&output_name),
                            Some(reason),
                        );
                        exclusive_hdr_output = None;
                        if let Some(output) = data.core.state.outputs.get_mut(&output_id) {
                            output.hdr_requested = false;
                            output.hdr_transition_target = None;
                        }
                    }
                    Err(err) => {
                        let reason = format!("HDR initial modeset setup failed: {err}");
                        flog_warn!(
                            "Exclusive HDR initial modeset failed on {output_name}: {err}; restoring the ordinary SDR topology"
                        );
                        persist_exclusive_hdr_phase(
                            ExclusiveHdrPhase::Failed,
                            Some(&output_name),
                            Some(&reason),
                        );
                        exclusive_hdr_output = None;
                        if let Some(output) = data.core.state.outputs.get_mut(&output_id) {
                            output.hdr_requested = false;
                            output.hdr_transition_target = None;
                        }
                    }
                }
            }

            surfaces.insert(crtc, surface);
            id += 1;
            next_x = next_x.max(origin.x + logical_size.w);
            flog("Output initialized (Wayland + DRM)");
            data.core.state.drm_submit_hw_cursor = true;
            initialized_one = true;
        }
    }

    if initialized_one {
        if crate::core::colord::refresh_all_output_colors(&mut data.core.state) {
            flog_warn!("colord: output colors refreshed after DRM init");
        }
    }

    let registration_token =
        loop_handle.insert_source(notifier, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => {
                let mut recover_exclusive = false;
                if let Some(device) = state.backend.devices.get_mut(&node) {
                    let exclusive_hdr_output = device.exclusive_hdr_output.clone();
                    let DrmDeviceState {
                        drm_output_manager,
                        surfaces,
                        ..
                    } = device;
                    if let Some(surface) = surfaces.get_mut(&crtc) {
                        let submitted = surface.drm_output.frame_submitted();
                        if let Err(err) = &submitted {
                            surface.stable_vblank_count = 0;
                            flog(&format!(
                                "frame_submitted failed on {:?}/{:?}: {}",
                                node, crtc, err
                            ));
                            if exclusive_hdr_output.as_deref()
                                == Some(surface.output.name().as_str())
                                && surface.hdr_enabled_applied
                            {
                                persist_exclusive_hdr_phase(
                                    ExclusiveHdrPhase::Failed,
                                    Some(surface.output.name().as_str()),
                                    Some("frame submission failed during HDR verification"),
                                );
                                recover_exclusive = true;
                            }
                        }
                        surface.frame_queued_at = None;
                        surface.hdr_commit_deadline = None;
                        if let Some(hdr_target) = surface.hdr_transition_target.take() {
                            let initial_modeset = surface.hdr_initial_modeset_pending;
                            surface.hdr_initial_modeset_pending = false;
                            let validation = submitted.map_err(anyhow::Error::from).and_then(|_| {
                                hdr_detection::hdr_kms::validate_connector_hdr_state(
                                    drm_output_manager.device(),
                                    surface.connector,
                                    hdr_target,
                                    surface.hdr_support.max_bpc.is_some(),
                                )
                            });
                            match validation {
                                Ok(snapshot) => {
                                    surface.hdr_enabled_applied = hdr_target;
                                    let verification_required = hdr_target
                                        && exclusive_hdr_output.as_deref()
                                            == Some(surface.output.name().as_str());
                                    if verification_required {
                                        surface.hdr_verify_vblank_count = 0;
                                        surface.hdr_verify_started_at = Some(Instant::now());
                                        persist_exclusive_hdr_phase(
                                            ExclusiveHdrPhase::Verifying,
                                            Some(surface.output.name().as_str()),
                                            None,
                                        );
                                        state
                                            .core
                                            .state
                                            .notify_runtime_display_status_changes();
                                    } else {
                                        surface.hdr_verify_vblank_count = 0;
                                        surface.hdr_verify_started_at = None;
                                    }
                                    if let Some(output) =
                                        state.core.state.outputs.get_mut(&surface.output_id)
                                    {
                                        output.hdr_transition_target = None;
                                        output.hdr_kms_applied = hdr_target;
                                        let render_active =
                                            crate::core::color::output_hdr_render_active(
                                                output.hdr_requested,
                                                output.hdr_supported,
                                                output.hdr_kms_applied,
                                            );
                                        output.hdr_verification_pending = verification_required;
                                        output.hdr_enabled = hdr_active_status_verified(
                                            render_active,
                                            verification_required,
                                            !verification_required,
                                        );
                                    }
                                    crate::core::wayland::color_management_protocol::notify_preferred_color_changed(
                                        &mut state.core.state,
                                    );
                                    flog_warn!(
                                        "HDR KMS {} validated on {}: active={hdr_target} properties={snapshot:?}",
                                        if initial_modeset {
                                            "initial modeset"
                                        } else {
                                            "live transition"
                                        },
                                        surface.output.name(),
                                    );
                                    if !hdr_target
                                        && exclusive_hdr_output.as_deref()
                                            == Some(surface.output.name().as_str())
                                        && load_exclusive_hdr_state().phase
                                            == ExclusiveHdrPhase::Failed
                                    {
                                        recover_exclusive = true;
                                    }
                                    if verification_required {
                                        // A scheduler wakeup without scene damage can be
                                        // coalesced by the retained renderer, leaving no frame
                                        // to submit and therefore no vblank to count. Force a
                                        // real scanout frame throughout the bounded verification
                                        // window.
                                        state.core.state.mark_output_full_damage(
                                            surface.output_id,
                                            DamageSource::Unknown,
                                        );
                                    }
                                }
                                Err(err) => {
                                    // Treat a partial or unverifiable property
                                    // application as unsafe. Mark the internal
                                    // state active solely to force an SDR reset
                                    // transaction on the next frame; userspace
                                    // rendering remains SDR throughout rollback.
                                    flog_warn!(
                                        "HDR KMS transition validation failed on {}: {err}; disabling HDR and staging SDR rollback",
                                        surface.output.name()
                                    );
                                    let persist = hdr_failure_persist_action(
                                        exclusive_hdr_output.as_deref(),
                                        surface.output.name().as_str(),
                                        nvidia_dual_head_hdr_allowed(),
                                    );
                                    apply_persisted_hdr_failure(
                                        persist,
                                        surface.output.name().as_str(),
                                    );
                                    if exclusive_hdr_output.as_deref()
                                        == Some(surface.output.name().as_str())
                                    {
                                        persist_exclusive_hdr_phase(
                                            ExclusiveHdrPhase::Failed,
                                            Some(surface.output.name().as_str()),
                                            Some(&format!(
                                                "HDR connector readback failed: {err}"
                                            )),
                                        );
                                        recover_exclusive = true;
                                    }
                                    surface.hdr_enabled_applied = true;
                                    if persist == HdrFailurePersist::DisableAll {
                                        for output in state.core.state.outputs.values_mut() {
                                            output.hdr_transition_target = None;
                                            output.hdr_kms_applied = false;
                                            clear_runtime_hdr_request(output);
                                        }
                                    } else if let Some(output) =
                                        state.core.state.outputs.get_mut(&surface.output_id)
                                    {
                                        output.hdr_transition_target = None;
                                        output.hdr_kms_applied = false;
                                        clear_runtime_hdr_request(output);
                                    }
                                    state.core.state.mark_redraw();
                                }
                            }
                        } else if submitted.is_ok() {
                            let baseline_was_still_warming =
                                surface.stable_vblank_count < HDR_MIN_STABLE_VBLANKS;
                            surface.stable_vblank_count = surface
                                .stable_vblank_count
                                .saturating_add(1)
                                .min(HDR_MIN_STABLE_VBLANKS);
                            if baseline_was_still_warming {
                                state.core.state.mark_redraw();
                            }

                            let now = Instant::now();
                            let verifying = surface.hdr_enabled_applied
                                && exclusive_hdr_output.as_deref()
                                    == Some(surface.output.name().as_str())
                                && surface.hdr_verify_started_at.is_some();
                            if verifying {
                                surface.hdr_verify_vblank_count =
                                    surface.hdr_verify_vblank_count.saturating_add(1);
                                if surface.hdr_verify_vblank_count % 60 == 0 {
                                    flog_warn!(
                                        "Exclusive HDR verification progress on {}: {}/{} successful PQ vblanks",
                                        surface.output.name(),
                                        surface.hdr_verify_vblank_count,
                                        HDR_VERIFY_VBLANKS
                                    );
                                }
                                let verification_complete = hdr_verification_complete(
                                    surface.hdr_verify_vblank_count,
                                    surface.hdr_verify_started_at,
                                    now,
                                );
                                if verification_complete {
                                    // `Some` is the authoritative in-progress marker. Clear it
                                    // only from the vblank callback that proves the final PQ
                                    // frame was actually presented; a wall-clock check in the
                                    // render loop must not stop one frame too early.
                                    surface.hdr_verify_started_at = None;
                                    if let Some(output) =
                                        state.core.state.outputs.get_mut(&surface.output_id)
                                    {
                                        let render_active =
                                            crate::core::color::output_hdr_render_active(
                                                output.hdr_requested,
                                                output.hdr_supported,
                                                output.hdr_kms_applied,
                                            );
                                        output.hdr_verification_pending = false;
                                        output.hdr_enabled =
                                            hdr_active_status_verified(render_active, true, true);
                                    }
                                    persist_exclusive_hdr_phase(
                                        ExclusiveHdrPhase::Active,
                                        Some(surface.output.name().as_str()),
                                        None,
                                    );
                                    enable_persisted_hdr_request(surface.output.name().as_str());
                                    crate::core::wayland::color_management_protocol::notify_preferred_color_changed(
                                        &mut state.core.state,
                                    );
                                    state.core.state.notify_runtime_display_status_changes();
                                    flog_warn!(
                                        "Exclusive HDR verified active on {} after at least {:?} and {} successful PQ vblanks",
                                        surface.output.name(),
                                        HDR_VERIFY_DURATION,
                                        HDR_VERIFY_VBLANKS
                                    );
                                } else {
                                    state.core.state.mark_output_full_damage(
                                        surface.output_id,
                                        DamageSource::Unknown,
                                    );
                                }
                            }
                        }
                    }
                }
                if recover_exclusive
                    && !state.exclusive_hdr_recovery_nodes.contains(&node)
                {
                    state.exclusive_hdr_recovery_nodes.push(node);
                }
            }
            DrmEvent::Error(err) => {
                flog(&format!("DRM event error on {:?}: {}", node, err));
            }
        })?;

    //data.backend.devices.insert(
    //    node,
    //    DrmDeviceState {
    //        registration_token,
    //        render_node: render_node_for_gpu,
    //       renderer,
    //         drm_output_manager,
    //         surfaces,
    //     },
    // );

    let temp_device = DrmDeviceState {
        registration_token,
        render_node: render_node_for_gpu,
        gpu_vendor_id,
        renderer,
        gbm: gbm.clone(),
        drm_output_manager,
        surfaces,
        exclusive_hdr_output,
    };

    let displays = collect_display_configs(&temp_device, &data.core, &configured_displays);

    if let Err(err) = write_display_config(&displays) {
        flog(&format!("Failed to write display config: {err}"));
    }

    if !displays.is_empty() {
        refresh_portal_services(&data.core.state.client_wayland_display);
    }

    data.backend.devices.insert(node, temp_device);

    Ok(())
}

//! [`wp_color_management_v1`](https://wayland.app/protocols/color-management-v1) server.

use crate::core::color::{
    primaries_plausible, primaries_wider_than, ColorDescription, ColorPrimaries,
    PrimariesChromaticity, RenderingIntent, SurfaceColorState,
    TransferFunction as CoreTransferFunction,
};
use crate::core::desktop::DesktopState;
use crate::core::icc::{self, parse_icc_profile, read_icc_from_fd};
use crate::core::wayland::client::ClientState;
use focaldesk_logging::flog_warn;
use focaldesk_types::OutputId;
use smithay::output::Output;
use smithay::wayland::compositor::{add_destruction_hook, with_states};
use std::collections::{HashMap, HashSet};
use std::os::fd::AsFd;
use std::sync::Mutex;
use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1, wp_color_management_surface_feedback_v1,
    wp_color_management_surface_v1, wp_color_manager_v1, wp_image_description_creator_icc_v1,
    wp_image_description_creator_params_v1, wp_image_description_info_v1, wp_image_description_v1,
};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{
    backend, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use std::sync::OnceLock;

fn wp_color_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("FOCALDESK_WP_COLOR_TRACE").ok().as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("on")
        )
    })
}

macro_rules! wp_color_trace {
    ($($arg:tt)*) => {
        if wp_color_trace_enabled() {
            wp_color_trace_impl(format!($($arg)*));
        }
    };
}

fn wp_color_trace_impl(msg: String) {
    use std::io::Write;

    let line = format!("[wp-color] {msg}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/focaldesk-wp-color.trace")
    {
        let _ = writeln!(file, "{line}");
    }
}

fn client_trace_prefix(client: &Client, dh: &DisplayHandle) -> String {
    match client.get_credentials(dh) {
        Ok(creds) => format!("client={:?} creds={creds:?}", client.id()),
        Err(err) => format!("client={:?} creds=<error:{err}>", client.id()),
    }
}

fn is_cursor_executable_name(exe_name: &str) -> bool {
    matches!(exe_name, "cursor" | "cursor-bin")
}

fn client_exe_basename(
    credentials: &crate::core::wayland::client::ClientCredentials,
) -> Option<String> {
    if let Ok(exe_path) = std::fs::read_link(format!("/proc/{}/exe", credentials.pid)) {
        if let Some(name) = exe_path.file_name() {
            return Some(name.to_string_lossy().into_owned());
        }
    }
    std::fs::read_to_string(format!("/proc/{}/comm", credentials.pid))
        .ok()
        .map(|comm| comm.trim().to_string())
}

fn client_is_cursor(client: &Client) -> bool {
    let Some(client_state) = client.get_data::<ClientState>() else {
        return false;
    };
    let Some(credentials) = client_state.credentials else {
        return false;
    };

    let Some(exe_name) = client_exe_basename(&credentials) else {
        return false;
    };

    is_cursor_executable_name(exe_name.as_ref())
}

fn send_image_description_ready(
    image: &wp_image_description_v1::WpImageDescriptionV1,
    identity: u64,
) {
    if image.version() >= 2 {
        image.ready2((identity >> 32) as u32, identity as u32);
    } else {
        image.ready(identity as u32);
    }
}

fn send_surface_feedback_preferred_changed(
    feedback: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
    identity: u64,
) {
    if feedback.version() >= 2 {
        feedback.preferred_changed2((identity >> 32) as u32, identity as u32);
    } else {
        feedback.preferred_changed(identity as u32);
    }
}

fn should_advertise_output_profiles(state: &DesktopState) -> bool {
    if state.outputs.values().any(|output| output.hdr_kms_applied) {
        return true;
    }
    if !crate::core::color::linear_sdr_runtime_enabled() {
        return false;
    }
    let any_wide_gamut_output = state
        .outputs
        .values()
        .any(|output| output.color_description.primaries != ColorDescription::SRGB.primaries);
    crate::core::color::wp_color_wide_gamut_enabled(
        state.render.chrome_shaders.output_encode_lut.is_some() || any_wide_gamut_output,
    )
}

fn output_uses_wide_gamut_description(state: &DesktopState, output_id: OutputId) -> bool {
    state.outputs.get(&output_id).is_some_and(|output| {
        output.hdr_kms_applied || output.color_description != ColorDescription::SRGB
    })
}

fn preferred_identity_for_output(state: &mut DesktopState, output_id: OutputId) -> u64 {
    if !should_advertise_output_profiles(state) {
        return state.color_management_state.canonical_sdr_identity();
    }
    if !output_uses_wide_gamut_description(state, output_id) {
        return state.color_management_state.canonical_sdr_identity();
    }
    state
        .color_management_state
        .preferred_identity_for_output(output_id)
}

fn refresh_preferred_identities(state: &mut DesktopState) {
    for output_id in state.outputs.keys().copied().collect::<Vec<_>>() {
        if should_advertise_output_profiles(state)
            && output_uses_wide_gamut_description(state, output_id)
        {
            state
                .color_management_state
                .refresh_preferred_identity(output_id);
        }
    }
}

fn send_preferred_changed_for_surface(state: &mut DesktopState, surface: &WlSurface) {
    let output_id = state.preferred_output_id_for_surface(surface);
    let identity = preferred_identity_for_output(state, output_id);
    for feedback in &state.color_management_state.surface_feedbacks {
        let Some(data) = feedback.data::<SurfaceColorFeedback>() else {
            continue;
        };
        if data.surface.id() == surface.id() {
            send_surface_feedback_preferred_changed(feedback, identity);
        }
    }
}

/// Notify surface feedback objects after an output profile change.
pub fn notify_preferred_color_changed(state: &mut DesktopState) {
    refresh_preferred_identities(state);
    let surfaces: Vec<WlSurface> = state
        .color_management_state
        .surface_feedbacks
        .iter()
        .filter_map(|feedback| {
            feedback
                .data::<SurfaceColorFeedback>()
                .map(|data| data.surface.clone())
        })
        .collect();
    for surface in surfaces {
        send_preferred_changed_for_surface(state, &surface);
    }
}

/// Re-send `preferred_changed` when a window moves to a different monitor.
pub fn notify_surface_feedback_preferred(state: &mut DesktopState, surface: &WlSurface) {
    send_preferred_changed_for_surface(state, surface);
}

/// Keep the widest output description seen so `get_preferred` does not flicker to sRGB mid-session.
pub fn note_output_color_resolved(state: &mut DesktopState, output_id: OutputId) {
    use focaldesk_settings_core::DisplayColorProfile;

    let description = state.output_color_description(output_id);
    if state.output_color_profile_override_for(output_id) == DisplayColorProfile::Srgb {
        state
            .color_management_state
            .preferred_output_descriptions
            .remove(&output_id);
        return;
    }
    if primaries_wider_than(description.primaries, ColorPrimaries::Srgb) {
        state
            .color_management_state
            .preferred_output_descriptions
            .insert(output_id, description);
    }
}

fn resolve_preferred_output_description(
    state: &mut DesktopState,
    output_id: OutputId,
) -> ColorDescription {
    if let Some(output) = state.outputs.get(&output_id) {
        if output.hdr_kms_applied {
            // Chrome rasters HDR in P3 + extended sRGB. 8-bit windows cannot
            // carry PQ, so prefer that raster space on P3-class panels.
            let peak = output.edid_hdr_max_luminance_nits.unwrap_or(1_000.0);
            let fall = output.edid_hdr_max_fall_nits.unwrap_or(peak);
            return ColorDescription::hdr_preferred_from_panel(
                output.color_description,
                peak,
                fall,
            );
        }
    }
    let description = state.output_color_description(output_id);
    if primaries_wider_than(description.primaries, ColorPrimaries::Srgb) {
        state
            .color_management_state
            .preferred_output_descriptions
            .insert(output_id, description);
        return description;
    }
    if let Some(&cached) = state
        .color_management_state
        .preferred_output_descriptions
        .get(&output_id)
    {
        if primaries_wider_than(cached.primaries, description.primaries) {
            wp_color_trace!(
                "get_preferred: using cached {:?} over transient {:?}",
                cached.primaries,
                description.primaries
            );
            return cached;
        }
    }
    description
}

#[derive(Default)]
pub struct ColorManagementState {
    pub surface_objects: HashSet<backend::ObjectId>,
    pub surface_feedbacks:
        Vec<wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1>,
    next_description_identity: u64,
    canonical_sdr_identity: Option<u64>,
    output_preferred_identities: HashMap<OutputId, u64>,
    /// Widest output description advertised this session; masks transient sRGB reload blips.
    pub preferred_output_descriptions: HashMap<OutputId, ColorDescription>,
    /// `wp_image_description_info_v1.done` is a destructor event; sending it inside
    /// `get_information` destroys the object before wayland-backend assigns child userdata.
    pending_info_done: Vec<wp_image_description_info_v1::WpImageDescriptionInfoV1>,
}

/// Send queued `wp_image_description_info_v1.done` events after `dispatch_clients` returns.
pub fn flush_pending_image_description_info_done(state: &mut DesktopState) {
    for info in state.color_management_state.pending_info_done.drain(..) {
        info.done();
    }
}

fn queue_image_description_info_done(
    info: wp_image_description_info_v1::WpImageDescriptionInfoV1,
    state: &mut DesktopState,
) {
    state.color_management_state.pending_info_done.push(info);
}

impl ColorManagementState {
    pub fn bind_global<D>(display: &DisplayHandle)
    where
        D: GlobalDispatch<wp_color_manager_v1::WpColorManagerV1, ()>
            + Dispatch<wp_color_manager_v1::WpColorManagerV1, ()>
            + Dispatch<
                wp_color_management_output_v1::WpColorManagementOutputV1,
                OutputColorManagement,
            > + Dispatch<
                wp_color_management_output_v1::WpColorManagementOutputV1,
                OrphanOutputColorManagement,
            > + Dispatch<
                wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
                SurfaceColorFeedback,
            > + Dispatch<
                wp_color_management_surface_v1::WpColorManagementSurfaceV1,
                SurfaceColorManagement,
            > + Dispatch<
                wp_color_management_surface_v1::WpColorManagementSurfaceV1,
                OrphanSurfaceColorManagement,
            > + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
            + Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()>
            + Dispatch<
                wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
                ParametricCreatorState,
            > + Dispatch<
                wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
                IccCreatorState,
            > + 'static,
    {
        wp_color_trace!("binding wp_color_management_v1 global");
        display.create_global::<D, wp_color_manager_v1::WpColorManagerV1, _>(2, ());
    }

    fn next_identity(&mut self) -> u64 {
        self.next_description_identity = self.next_description_identity.wrapping_add(1).max(1);
        self.next_description_identity
    }

    pub(crate) fn next_description_identity(&mut self) -> u64 {
        self.next_identity()
    }

    fn canonical_sdr_identity(&mut self) -> u64 {
        if let Some(id) = self.canonical_sdr_identity {
            return id;
        }
        let id = self.next_identity();
        self.canonical_sdr_identity = Some(id);
        id
    }

    fn preferred_identity_for_output(&mut self, output_id: OutputId) -> u64 {
        if let Some(id) = self.output_preferred_identities.get(&output_id) {
            return *id;
        }
        let id = self.next_identity();
        self.output_preferred_identities.insert(output_id, id);
        id
    }

    fn refresh_preferred_identity(&mut self, output_id: OutputId) -> u64 {
        let id = self.next_identity();
        self.output_preferred_identities.insert(output_id, id);
        id
    }
}

#[derive(Debug, Clone)]
pub struct ImageDescriptionData {
    pub identity: u64,
    pub description: ColorDescription,
    pub ready: bool,
    pub allows_information: bool,
    pub icc_profile: Option<Vec<u8>>,
    /// When true, `get_information` always advertises canonical sRGB (KMS scanout path).
    pub advertise_as_canonical_sdr: bool,
}

#[derive(Default)]
pub struct IccCreatorState {
    inner: Mutex<IccCreatorInner>,
}

#[derive(Default)]
struct IccCreatorInner {
    icc_bytes: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct ParametricCreatorState {
    inner: Mutex<ParametricCreatorInner>,
}

#[derive(Debug, Default)]
struct ParametricCreatorInner {
    tf: Option<ParametricTransfer>,
    primaries: Option<ColorPrimaries>,
    /// Protocol units: minimum is cd/m² × 10000; maximum/reference are cd/m².
    min_luminance_x10000: Option<u32>,
    max_luminance_nits: Option<u32>,
    reference_luminance_nits: Option<u32>,
    mastering_primaries: Option<ColorPrimaries>,
    mastering_min_luminance_x10000: Option<u32>,
    mastering_max_luminance_nits: Option<u32>,
    max_cll_nits: Option<u32>,
    max_fall_nits: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
enum ParametricTransfer {
    Named(wp_color_manager_v1::TransferFunction),
    Power(f32),
}

pub struct OutputColorManagement {
    pub output: Output,
}

/// Placeholder when `get_output` receives an invalid `wl_output` but Wayland still
/// allocated the `NewId`.
pub struct OrphanOutputColorManagement;

/// Placeholder object when `get_surface` hits `surface_exists` but Wayland still
/// allocates the `NewId`.
pub struct OrphanSurfaceColorManagement;

pub struct SurfaceColorFeedback {
    pub(crate) surface: WlSurface,
}

impl SurfaceColorFeedback {
    fn new(surface: WlSurface) -> Self {
        Self { surface }
    }

    fn is_inert(&self) -> bool {
        !self.surface.is_alive()
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceColorManagement {
    surface: WlSurface,
}

impl SurfaceColorManagement {
    fn new(surface: WlSurface) -> Self {
        add_destruction_hook::<DesktopState, _>(&surface, |state, surface| {
            state
                .color_management_state
                .surface_objects
                .remove(&surface.id());
            state.refresh_surface_color(surface);
        });
        Self { surface }
    }

    fn is_inert(&self) -> bool {
        !self.surface.is_alive()
    }
}

fn windows_scrgb_supported() -> bool {
    crate::core::color::linear_sdr_runtime_enabled()
}

fn send_manager_advertisement(manager: &wp_color_manager_v1::WpColorManagerV1) {
    use wp_color_manager_v1::{Feature, Primaries, RenderIntent, TransferFunction};

    manager.supported_intent(RenderIntent::Perceptual);
    manager.supported_intent(RenderIntent::Relative);

    manager.supported_feature(Feature::IccV2V4);
    manager.supported_feature(Feature::Parametric);
    manager.supported_feature(Feature::SetPrimaries);
    manager.supported_feature(Feature::SetTfPower);
    manager.supported_feature(Feature::SetLuminances);
    manager.supported_feature(Feature::SetMasteringDisplayPrimaries);
    manager.supported_feature(Feature::ExtendedTargetVolume);
    if windows_scrgb_supported() {
        manager.supported_feature(Feature::WindowsScrgb);
    }

    manager.supported_tf_named(TransferFunction::Bt1886);
    manager.supported_tf_named(TransferFunction::Gamma22);
    manager.supported_tf_named(TransferFunction::ExtLinear);
    manager.supported_tf_named(TransferFunction::St2084Pq);
    // Chromium maps gfx::ColorSpace::SRGB → WP_COLOR_MANAGER_V1_TRANSFER_FUNCTION_SRGB
    // and SRGB_HDR → EXT_SRGB. Without ExtSrgb it copies the preferred PQ
    // description onto linear/sRGB-HDR window buffers.
    manager.supported_tf_named(TransferFunction::Srgb);
    manager.supported_tf_named(TransferFunction::ExtSrgb);
    manager.supported_tf_named(TransferFunction::CompoundPower24);

    manager.supported_primaries_named(Primaries::Srgb);
    manager.supported_primaries_named(Primaries::DisplayP3);
    manager.supported_primaries_named(Primaries::Bt2020);
    manager.done();
    wp_color_trace!("manager advertisement sent");
}

fn fail_image_description(
    image_description: &wp_image_description_v1::WpImageDescriptionV1,
    msg: impl Into<String>,
) {
    image_description.failed(wp_image_description_v1::Cause::Unsupported, msg.into());
}

fn init_failed_image_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
    msg: impl Into<String>,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let msg = msg.into();
    wp_color_trace!("image description init failed: {msg}");
    let image = data_init.init(
        id,
        ImageDescriptionData {
            identity: 0,
            description: ColorDescription::SRGB,
            ready: false,
            allows_information: false,
            icc_profile: None,
            advertise_as_canonical_sdr: false,
        },
    );
    fail_image_description(&image, msg);
}

fn finish_image_description<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
    description: ColorDescription,
    allows_information: bool,
    icc_profile: Option<Vec<u8>>,
    advertise_as_canonical_sdr: bool,
    identity: Option<u64>,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = identity.unwrap_or_else(|| state.color_management_state.next_identity());
    wp_color_trace!(
        "image description finished: id={identity} ready=true allow_info={allows_information} canonical_sdr={advertise_as_canonical_sdr} desc={:?}",
        description
    );
    let image = data_init.init(
        id,
        ImageDescriptionData {
            identity,
            description,
            ready: true,
            allows_information,
            icc_profile,
            advertise_as_canonical_sdr,
        },
    );
    send_image_description_ready(&image, identity);
}

fn finish_canonical_sdr_image_description<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
    description: ColorDescription,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = state.color_management_state.canonical_sdr_identity();
    wp_color_trace!(
        "canonical output image description finished: id={identity} desc={description:?}"
    );
    let image = data_init.init(
        id,
        ImageDescriptionData {
            identity,
            description,
            ready: true,
            allows_information: true,
            icc_profile: None,
            advertise_as_canonical_sdr: true,
        },
    );
    send_image_description_ready(&image, identity);
}

fn finish_output_image_description<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
    description: ColorDescription,
    icc_profile: Option<Vec<u8>>,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let has_icc_profile = icc_profile.as_ref().is_some_and(|icc| !icc.is_empty());

    if should_advertise_output_profiles(state) && has_icc_profile {
        finish_image_description(
            state,
            data_init,
            id,
            output_advertised_description(description),
            true,
            icc_profile,
            false,
            None,
        );
    } else if should_advertise_output_profiles(state) && description != ColorDescription::SRGB {
        finish_image_description(
            state,
            data_init,
            id,
            output_advertised_description(description),
            true,
            None,
            false,
            None,
        );
    } else {
        finish_canonical_sdr_image_description(state, data_init, id, ColorDescription::SRGB);
    }
}

fn finish_preferred_output_image_description<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
    output_id: OutputId,
    description: ColorDescription,
    icc_profile: Option<Vec<u8>>,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = Some(preferred_identity_for_output(state, output_id));
    let has_icc_profile = icc_profile.as_ref().is_some_and(|icc| !icc.is_empty());

    if should_advertise_output_profiles(state) && has_icc_profile {
        // Use the output ICC description as-is (primaries + transfer from colord).
        finish_image_description(
            state,
            data_init,
            id,
            output_advertised_description(description),
            true,
            icc_profile,
            false,
            identity,
        );
    } else if should_advertise_output_profiles(state) && description != ColorDescription::SRGB {
        finish_image_description(
            state,
            data_init,
            id,
            output_advertised_description(description),
            true,
            None,
            false,
            identity,
        );
    } else {
        finish_canonical_sdr_image_description(state, data_init, id, ColorDescription::SRGB);
    }
}

fn init_inert_image_description_info<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    information: New<wp_image_description_info_v1::WpImageDescriptionInfoV1>,
) where
    D: Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> + 'static,
{
    let info = data_init.init(information, ());
    queue_image_description_info_done(info, state);
}

fn primary_luminance_wire_values(description: &ColorDescription) -> (u32, u32, u32) {
    // Minimum is wire ×10000; maximum/reference are unscaled cd/m².
    let pq = description.transfer == CoreTransferFunction::St2084Pq;
    let hdr_volume = crate::core::color::is_hdr_client_preferred_transfer(description.transfer)
        || description.is_windows_scrgb();
    let min_nits: f32 = if pq { 0.005 } else { 0.2 };
    let min_lum = (min_nits * 10_000.0).round() as u32;
    // ST 2084 and extended sRGB advertise a 10,000-nit signal volume. The
    // actual display/content peak is a target-volume property and is emitted
    // separately below. Advertising the panel peak here causes clients such
    // as Chromium to construct a malformed HDR output color space when mapping
    // SDR video into the HDR surface.
    let max_lum = if hdr_volume {
        10_000
    } else {
        description.max_luminance_nits.round().max(1.0) as u32
    };
    // Windows-scRGB unit white is 80 cd/m². Assumed graphics white (203 nits)
    // is sample 2.5375 and must not be advertised as the luminance of (1,1,1),
    // or Chromium maps SDR images as if 1.0 were paper white.
    let reference_lum = if description.is_windows_scrgb() {
        80
    } else {
        description.reference_white_nits.round().max(1.0) as u32
    };
    (min_lum, max_lum, reference_lum)
}

fn target_luminance_wire_values(description: &ColorDescription) -> (u32, u32) {
    let min_nits: f32 = if description.transfer == CoreTransferFunction::St2084Pq {
        0.005
    } else {
        0.2
    };
    (
        (min_nits * 10_000.0).round() as u32,
        description.max_luminance_nits.round().max(1.0) as u32,
    )
}

fn primaries_from_wire(
    r_x: i32,
    r_y: i32,
    g_x: i32,
    g_y: i32,
    b_x: i32,
    b_y: i32,
    w_x: i32,
    w_y: i32,
) -> Option<PrimariesChromaticity> {
    // color-management-v1 carries CIE xy coordinates multiplied by one
    // million.  Using 1e5 here rejected Chromium's valid custom primaries and
    // silently collapsed those buffers back to the output color space.
    let scale = 1_000_000.0f32;
    let ch = PrimariesChromaticity {
        r: [r_x as f32 / scale, r_y as f32 / scale],
        g: [g_x as f32 / scale, g_y as f32 / scale],
        b: [b_x as f32 / scale, b_y as f32 / scale],
        w: [w_x as f32 / scale, w_y as f32 / scale],
    };
    primaries_plausible(&ch).then_some(ch)
}

fn sanitize_client_color_description(
    _state: &DesktopState,
    surface: &WlSurface,
    description: ColorDescription,
) -> ColorDescription {
    if description.transfer == CoreTransferFunction::St2084Pq {
        flog_warn!(
            "wp color: surface={:?} tagged PQ; 8-bit decodes as P3 sRGB-HDR, 10-bit keeps PQ, FP16 decodes as Rec.709 linear HDR",
            surface.id()
        );
    }
    description
}

/// Normalize output/preferred descriptions for client queries (ICC gamma → sRGB-class TF).
fn output_advertised_description(description: ColorDescription) -> ColorDescription {
    // Chrome rejects Bt1886/Gamma22/ExtLinear for internal BT709/sRGB paths ("non-power-curve").
    // Advertise sRGB-class transfer to clients; ICC LUT scanout still uses the profile TRC.
    ColorDescription {
        transfer: if crate::core::color::is_hdr_client_preferred_transfer(description.transfer) {
            description.transfer
        } else {
            CoreTransferFunction::Srgb
        },
        ..description
    }
}

fn send_canonical_sdr_image_description_info(
    info: wp_image_description_info_v1::WpImageDescriptionInfoV1,
    state: &mut DesktopState,
) {
    use wp_color_manager_v1::{Primaries, TransferFunction};

    info.primaries_named(Primaries::Srgb);
    info.tf_named(TransferFunction::CompoundPower24);
    let (min_lum, max_lum, reference_lum) = primary_luminance_wire_values(&ColorDescription::SRGB);
    info.luminances(min_lum, max_lum, reference_lum);
    if info.version() >= 2 {
        info.target_luminance(min_lum, max_lum);
    }
    queue_image_description_info_done(info, state);
}

fn emit_image_description_info_events(
    info: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
    description: &ColorDescription,
) {
    use wp_color_manager_v1::{Primaries, TransferFunction};

    let use_custom = match description.primaries {
        ColorPrimaries::Custom(ch) => primaries_plausible(&ch),
        _ => false,
    };

    match description.primaries {
        ColorPrimaries::Srgb if !use_custom => info.primaries_named(Primaries::Srgb),
        ColorPrimaries::DisplayP3 if !use_custom => info.primaries_named(Primaries::DisplayP3),
        ColorPrimaries::Bt2020 if !use_custom => info.primaries_named(Primaries::Bt2020),
        ColorPrimaries::Custom(ch) if use_custom => {
            info.primaries(
                (ch.r[0] * 1_000_000.0).round() as i32,
                (ch.r[1] * 1_000_000.0).round() as i32,
                (ch.g[0] * 1_000_000.0).round() as i32,
                (ch.g[1] * 1_000_000.0).round() as i32,
                (ch.b[0] * 1_000_000.0).round() as i32,
                (ch.b[1] * 1_000_000.0).round() as i32,
                (ch.w[0] * 1_000_000.0).round() as i32,
                (ch.w[1] * 1_000_000.0).round() as i32,
            );
        }
        _ => {
            wp_color_trace!(
                "get_information: invalid output primaries, falling back to sRGB advertisement",
            );
            info.primaries_named(Primaries::Srgb);
        }
    }

    let tf_named = match description.transfer {
        CoreTransferFunction::Srgb => TransferFunction::Srgb,
        CoreTransferFunction::SrgbHdr => TransferFunction::ExtSrgb,
        CoreTransferFunction::Bt1886 | CoreTransferFunction::Gamma22 => {
            TransferFunction::CompoundPower24
        }
        CoreTransferFunction::Linear => TransferFunction::ExtLinear,
        CoreTransferFunction::St2084Pq => TransferFunction::St2084Pq,
    };
    info.tf_named(tf_named);

    let (primary_min_lum, primary_max_lum, reference_lum) =
        primary_luminance_wire_values(description);
    info.luminances(primary_min_lum, primary_max_lum, reference_lum);
    if info.version() >= 2
        && crate::core::color::is_hdr_client_preferred_transfer(description.transfer)
    {
        // Chromium uses target primaries to construct the HDR display
        // color volume. Omitting this event leaves its HDR target
        // metadata incomplete, which can collapse P3 content and produce
        // an incorrect paper-white mapping.
        let target = description.primaries.chromaticity();
        info.target_primaries(
            (target.r[0] * 1_000_000.0).round() as i32,
            (target.r[1] * 1_000_000.0).round() as i32,
            (target.g[0] * 1_000_000.0).round() as i32,
            (target.g[1] * 1_000_000.0).round() as i32,
            (target.b[0] * 1_000_000.0).round() as i32,
            (target.b[1] * 1_000_000.0).round() as i32,
            (target.w[0] * 1_000_000.0).round() as i32,
            (target.w[1] * 1_000_000.0).round() as i32,
        );
    }
    // The internal description's max/CLL/FALL represent the content or
    // display target volume. PQ's primary volume remains the fixed
    // 10,000-nit ST 2084 signal swing, while target_luminance carries the
    // real panel/content peak.
    if info.version() >= 2 {
        let (target_min_lum, target_max_lum) = target_luminance_wire_values(description);
        info.target_luminance(target_min_lum, target_max_lum);
        if let Some(max_cll) = description.max_cll_nits {
            info.target_max_cll(max_cll.round().max(1.0) as u32);
        }
        if let Some(max_fall) = description.max_fall_nits {
            info.target_max_fall(max_fall.round().max(1.0) as u32);
        }
    }
}

fn send_image_description_info(
    info: wp_image_description_info_v1::WpImageDescriptionInfoV1,
    description: &ColorDescription,
    state: &mut DesktopState,
) {
    emit_image_description_info_events(&info, description);
    queue_image_description_info_done(info, state);
}

fn build_description_from_params(
    creator: &ParametricCreatorInner,
) -> Result<ColorDescription, String> {
    let primaries = creator
        .primaries
        .ok_or_else(|| "missing primaries".to_string())?;

    let transfer = creator
        .tf
        .ok_or_else(|| "missing transfer function".to_string())?;

    let mapped_tf = match transfer {
        ParametricTransfer::Named(tf) => match tf {
            wp_color_manager_v1::TransferFunction::Bt1886 => CoreTransferFunction::Bt1886,
            wp_color_manager_v1::TransferFunction::Gamma22 => CoreTransferFunction::Gamma22,
            wp_color_manager_v1::TransferFunction::Srgb
            | wp_color_manager_v1::TransferFunction::CompoundPower24 => CoreTransferFunction::Srgb,
            wp_color_manager_v1::TransferFunction::ExtSrgb => CoreTransferFunction::SrgbHdr,
            wp_color_manager_v1::TransferFunction::ExtLinear => CoreTransferFunction::Linear,
            wp_color_manager_v1::TransferFunction::St2084Pq => CoreTransferFunction::St2084Pq,
            _ => return Err("unsupported named transfer function".into()),
        },
        ParametricTransfer::Power(exp) if (exp - 2.4).abs() <= 0.0001 => CoreTransferFunction::Srgb,
        ParametricTransfer::Power(exp) if (exp - 2.2).abs() <= 0.0001 => {
            CoreTransferFunction::Gamma22
        }
        ParametricTransfer::Power(_) => return Err("unsupported power transfer function".into()),
    };

    let default_ref_nits = if mapped_tf == CoreTransferFunction::St2084Pq
        || mapped_tf == CoreTransferFunction::SrgbHdr
    {
        203.0
    } else {
        80.0
    };
    let ref_nits = creator
        .reference_luminance_nits
        .map(|nits| nits as f32)
        .unwrap_or(default_ref_nits);
    let primary_min_nits = creator
        .min_luminance_x10000
        .map(|value| value as f32 / 10_000.0)
        .unwrap_or(if mapped_tf == CoreTransferFunction::St2084Pq {
            0.005
        } else {
            0.2
        });
    let primary_max_nits = if mapped_tf == CoreTransferFunction::St2084Pq {
        // color-management-v1 defines PQ's primary volume as a fixed
        // 10,000-nit swing; max_lum is ignored for this transfer function.
        primary_min_nits + 10_000.0
    } else {
        creator
            .max_luminance_nits
            .map(|nits| nits as f32)
            .unwrap_or(ref_nits)
    };
    let mastering_peak = creator.mastering_max_luminance_nits.map(|nits| nits as f32);
    let max_nits = mastering_peak.unwrap_or(primary_max_nits);

    Ok(ColorDescription {
        primaries,
        transfer: mapped_tf,
        reference_white_nits: ref_nits,
        max_luminance_nits: max_nits,
        max_cll_nits: creator.max_cll_nits.map(|nits| nits as f32),
        max_fall_nits: creator.max_fall_nits.map(|nits| nits as f32),
        windows_scrgb_stimulus: mapped_tf == CoreTransferFunction::Linear
            && matches!(primaries, crate::core::color::ColorPrimaries::Srgb)
            && (ref_nits - 80.0).abs() <= 1.0
            && max_nits >= 1_000.0,
    })
}

fn intent_from_wire(value: WEnum<wp_color_manager_v1::RenderIntent>) -> Option<RenderingIntent> {
    match value {
        WEnum::Value(wp_color_manager_v1::RenderIntent::Perceptual) => {
            Some(RenderingIntent::Perceptual)
        }
        WEnum::Value(wp_color_manager_v1::RenderIntent::Relative) => {
            Some(RenderingIntent::Relative)
        }
        WEnum::Value(wp_color_manager_v1::RenderIntent::Absolute) => {
            Some(RenderingIntent::Absolute)
        }
        WEnum::Value(_) | WEnum::Unknown(_) => None,
    }
}

fn apply_surface_description(
    state: &mut DesktopState,
    surface: &WlSurface,
    description: Option<ColorDescription>,
    intent: RenderingIntent,
) {
    let description =
        description.map(|desc| sanitize_client_color_description(state, surface, desc));

    wp_color_trace!(
        "apply surface description: surface={:?} desc={:?} intent={intent:?}",
        surface.id(),
        description
    );
    with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceColorState>()
            .pending()
            .description = description;
        states
            .cached_state
            .get::<SurfaceColorState>()
            .pending()
            .intent = intent;
    });
    state.refresh_surface_color(surface);
}

fn tf_into_named(
    value: WEnum<wp_color_manager_v1::TransferFunction>,
) -> Option<wp_color_manager_v1::TransferFunction> {
    match value {
        WEnum::Value(tf) => Some(tf),
        WEnum::Unknown(_) => None,
    }
}

fn post_creator_error(
    resource: &wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    error: wp_image_description_creator_params_v1::Error,
    msg: &str,
) {
    resource.post_error(error, msg);
}

impl GlobalDispatch<wp_color_manager_v1::WpColorManagerV1, ()> for DesktopState {
    fn can_view(client: Client, _global_data: &()) -> bool {
        // Cursor still hits incomplete paths; browsers need wp_color (Chrome binds as `chrome`).
        !client_is_cursor(&client)
    }

    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        client: &Client,
        resource: New<wp_color_manager_v1::WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        wp_color_trace!(
            "wp_color_manager_v1 bound {}",
            client_trace_prefix(client, _handle)
        );
        send_manager_advertisement(&manager);
    }
}

impl Dispatch<wp_color_manager_v1::WpColorManagerV1, ()> for DesktopState {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_color_manager_v1::WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!("manager {} {request:?}", client_trace_prefix(client, dh));
        match request {
            wp_color_manager_v1::Request::Destroy => {}
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                wp_color_trace!(
                    "manager get_surface: {} surface={:?}",
                    client_trace_prefix(client, dh),
                    surface.id()
                );
                if state
                    .color_management_state
                    .surface_objects
                    .contains(&surface.id())
                {
                    data_init.init(id, OrphanSurfaceColorManagement);
                    resource.post_error(
                        wp_color_manager_v1::Error::SurfaceExists,
                        "surface already has a wp_color_management_surface_v1 object",
                    );
                    return;
                }
                state
                    .color_management_state
                    .surface_objects
                    .insert(surface.id());
                data_init.init(id, SurfaceColorManagement::new(surface));
            }
            wp_color_manager_v1::Request::GetOutput { id, output } => {
                wp_color_trace!(
                    "manager get_output: {} output={:?}",
                    client_trace_prefix(client, dh),
                    output.id()
                );
                let Some(output_handle) = Output::from_resource(&output) else {
                    wp_color_trace!(
                        "manager get_output rejected: {} invalid wl_output={:?}",
                        client_trace_prefix(client, dh),
                        output.id()
                    );
                    data_init.init(id, OrphanOutputColorManagement);
                    resource.post_error(
                        wp_color_manager_v1::Error::UnsupportedFeature,
                        "invalid wl_output",
                    );
                    return;
                };
                data_init.init(
                    id,
                    OutputColorManagement {
                        output: output_handle,
                    },
                );
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                wp_color_trace!(
                    "manager get_surface_feedback: {} surface={:?}",
                    client_trace_prefix(client, dh),
                    surface.id()
                );
                let feedback = data_init.init(id, SurfaceColorFeedback::new(surface.clone()));
                send_preferred_changed_for_surface(state, &surface);
                state
                    .color_management_state
                    .surface_feedbacks
                    .push(feedback);
            }
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                wp_color_trace!(
                    "manager create_parametric_creator: {}",
                    client_trace_prefix(client, dh)
                );
                data_init.init(obj, ParametricCreatorState::default());
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                wp_color_trace!(
                    "manager create_icc_creator: {}",
                    client_trace_prefix(client, dh)
                );
                data_init.init(obj, IccCreatorState::default());
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                wp_color_trace!(
                    "manager create_windows_scrgb: {}",
                    client_trace_prefix(client, dh)
                );
                if !windows_scrgb_supported() {
                    init_failed_image_description(
                        data_init,
                        image_description,
                        "Windows-scRGB requires linear compositing (FOCALDESK_LINEAR_SDR=0?)",
                    );
                    resource.post_error(
                        wp_color_manager_v1::Error::UnsupportedFeature,
                        "windows_scrgb not supported",
                    );
                    return;
                }
                finish_image_description(
                    state,
                    data_init,
                    image_description,
                    ColorDescription::WINDOWS_SCRGB,
                    false,
                    None,
                    false,
                    None,
                );
            }
            wp_color_manager_v1::Request::GetImageDescription {
                image_description,
                reference: _,
            } => {
                wp_color_trace!(
                    "manager get_image_description rejected: {}",
                    client_trace_prefix(client, dh)
                );
                init_failed_image_description(
                    data_init,
                    image_description,
                    "image description references are not supported",
                );
            }
            other => {
                wp_color_trace!("manager unhandled: {other:?}");
            }
        }
    }
}

fn post_icc_creator_error(
    resource: &wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    error: wp_image_description_creator_icc_v1::Error,
    msg: &str,
) {
    resource.post_error(error, msg);
}

impl Dispatch<wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1, IccCreatorState>
    for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
        request: wp_image_description_creator_icc_v1::Request,
        creator: &IccCreatorState,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!(
            "icc creator {} {request:?}",
            client_trace_prefix(client, dh)
        );
        match request {
            wp_image_description_creator_icc_v1::Request::SetIccFile {
                icc_profile,
                offset,
                length,
            } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.icc_bytes.is_some() {
                    post_icc_creator_error(
                        resource,
                        wp_image_description_creator_icc_v1::Error::AlreadySet,
                        "ICC file already set",
                    );
                    return;
                }
                match read_icc_from_fd(icc_profile, offset, length) {
                    Ok(bytes) => {
                        inner.icc_bytes = Some(bytes);
                    }
                    Err(crate::core::icc::IccError::Invalid("bad size")) => {
                        post_icc_creator_error(
                            resource,
                            wp_image_description_creator_icc_v1::Error::BadSize,
                            "invalid ICC size",
                        );
                    }
                    Err(crate::core::icc::IccError::Invalid("out of file")) => {
                        post_icc_creator_error(
                            resource,
                            wp_image_description_creator_icc_v1::Error::OutOfFile,
                            "ICC range exceeds file size",
                        );
                    }
                    Err(_) => {
                        post_icc_creator_error(
                            resource,
                            wp_image_description_creator_icc_v1::Error::BadFd,
                            "ICC fd not readable or seekable",
                        );
                    }
                }
            }
            wp_image_description_creator_icc_v1::Request::Create { image_description } => {
                let bytes = {
                    let inner = creator.inner.lock().unwrap();
                    match inner.icc_bytes.clone() {
                        Some(bytes) => bytes,
                        None => {
                            init_failed_image_description(
                                data_init,
                                image_description,
                                "ICC file not set",
                            );
                            post_icc_creator_error(
                                resource,
                                wp_image_description_creator_icc_v1::Error::IncompleteSet,
                                "ICC file not set",
                            );
                            return;
                        }
                    }
                };
                match parse_icc_profile(&bytes) {
                    Ok(parsed) => {
                        finish_image_description(
                            state,
                            data_init,
                            image_description,
                            parsed.description,
                            false,
                            None,
                            false,
                            None,
                        );
                    }
                    Err(err) => {
                        wp_color_trace!("ICC parse failed: {err:?}");
                        init_failed_image_description(
                            data_init,
                            image_description,
                            "unsupported ICC profile",
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

impl
    Dispatch<
        wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        ParametricCreatorState,
    > for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        creator: &ParametricCreatorState,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!(
            "parametric creator {} {request:?}",
            client_trace_prefix(client, dh)
        );
        match request {
            wp_image_description_creator_params_v1::Request::SetTfNamed { tf } => {
                wp_color_trace!("parametric creator set_tf_named");
                let Some(tf) = tf_into_named(tf) else {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::InvalidTf,
                        "invalid tf",
                    );
                    return;
                };
                let mut inner = creator.inner.lock().unwrap();
                if inner.tf.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "transfer function already set",
                    );
                    return;
                }
                inner.tf = Some(ParametricTransfer::Named(tf));
            }
            wp_image_description_creator_params_v1::Request::SetTfPower { eexp } => {
                wp_color_trace!("parametric creator set_tf_power: eexp={eexp}");
                let mut inner = creator.inner.lock().unwrap();
                if inner.tf.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "transfer function already set",
                    );
                    return;
                }
                let exp = eexp as f32 / 10_000.0;
                if !(1.0..=10.0).contains(&exp) {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::InvalidTf,
                        "invalid tf exponent",
                    );
                    return;
                }
                inner.tf = Some(ParametricTransfer::Power(exp));
            }
            wp_image_description_creator_params_v1::Request::SetPrimariesNamed { primaries } => {
                wp_color_trace!("parametric creator set_primaries_named: primaries={primaries:?}");
                let mapped = match primaries {
                    WEnum::Value(p) => ColorPrimaries::from_wp_named(p).unwrap_or_else(|| {
                        wp_color_trace!(
                            "parametric creator set_primaries_named: unsupported named primaries, falling back to sRGB",
                        );
                        ColorPrimaries::Srgb
                    }),
                    WEnum::Unknown(_) => {
                        wp_color_trace!(
                            "parametric creator set_primaries_named: unknown primaries enum, falling back to sRGB",
                        );
                        ColorPrimaries::Srgb
                    }
                };
                let mut inner = creator.inner.lock().unwrap();
                if inner.primaries.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "primaries already set",
                    );
                    return;
                }
                inner.primaries = Some(mapped);
            }
            wp_image_description_creator_params_v1::Request::Create { image_description } => {
                wp_color_trace!("parametric creator create");
                let inner = creator.inner.lock().unwrap();
                let description = match build_description_from_params(&inner) {
                    Ok(description) => description,
                    Err(msg) => {
                        init_failed_image_description(data_init, image_description, msg);
                        return;
                    }
                };
                drop(inner);
                finish_image_description(
                    state,
                    data_init,
                    image_description,
                    description,
                    true,
                    None,
                    false,
                    None,
                );
            }
            wp_image_description_creator_params_v1::Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let primaries = match primaries_from_wire(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y) {
                    Some(ch) => ColorPrimaries::Custom(ch),
                    None => {
                        wp_color_trace!(
                            "parametric creator set_primaries: invalid wire values, falling back to sRGB",
                        );
                        ColorPrimaries::Srgb
                    }
                };
                let mut inner = creator.inner.lock().unwrap();
                if inner.primaries.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "primaries already set",
                    );
                    return;
                }
                inner.primaries = Some(primaries);
            }
            wp_image_description_creator_params_v1::Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.reference_luminance_nits.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "luminances already set",
                    );
                    return;
                }
                inner.min_luminance_x10000 = Some(min_lum);
                inner.max_luminance_nits = Some(max_lum);
                inner.reference_luminance_nits = Some(reference_lum);
            }
            wp_image_description_creator_params_v1::Request::SetMasteringDisplayPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let mastering = primaries_from_wire(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y)
                    .map(ColorPrimaries::Custom);
                let mut inner = creator.inner.lock().unwrap();
                if inner.mastering_primaries.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "mastering display primaries already set",
                    );
                    return;
                }
                // Invalid custom target coordinates make the eventual image
                // description fail instead of terminating the client here.
                inner.mastering_primaries = mastering;
            }
            wp_image_description_creator_params_v1::Request::SetMasteringLuminance {
                min_lum,
                max_lum,
            } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.mastering_max_luminance_nits.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "mastering luminance already set",
                    );
                    return;
                }
                inner.mastering_min_luminance_x10000 = Some(min_lum);
                inner.mastering_max_luminance_nits = Some(max_lum);
            }
            wp_image_description_creator_params_v1::Request::SetMaxCll { max_cll } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.max_cll_nits.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "max_cll already set",
                    );
                    return;
                }
                inner.max_cll_nits = Some(max_cll);
            }
            wp_image_description_creator_params_v1::Request::SetMaxFall { max_fall } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.max_fall_nits.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "max_fall already set",
                    );
                    return;
                }
                inner.max_fall_nits = Some(max_fall);
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
    for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_image_description_v1::WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &ImageDescriptionData,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!(
            "image description {} {request:?}",
            client_trace_prefix(client, dh)
        );
        match request {
            wp_image_description_v1::Request::Destroy => {}
            wp_image_description_v1::Request::GetInformation { information } => {
                if !data.ready {
                    init_inert_image_description_info(state, data_init, information);
                    resource.post_error(
                        wp_image_description_v1::Error::NotReady,
                        "image description not ready",
                    );
                    return;
                }
                if !data.allows_information {
                    init_inert_image_description_info(state, data_init, information);
                    resource.post_error(
                        wp_image_description_v1::Error::NoInformation,
                        "image description info unavailable",
                    );
                    return;
                }
                let info = data_init.init(information, ());
                if data.advertise_as_canonical_sdr {
                    send_canonical_sdr_image_description_info(info, state);
                } else if let Some(icc) = &data.icc_profile {
                    // Chromium ignores icc_file (NOTIMPLEMENTED) and needs parametric
                    // primaries/tf/luminances to learn output gamut.
                    emit_image_description_info_events(&info, &data.description);
                    match icc::memfd_from_bytes(icc) {
                        Ok(fd) => {
                            info.icc_file(fd.as_fd(), icc.len() as u32);
                            queue_image_description_info_done(info, state);
                        }
                        Err(err) => {
                            wp_color_trace!("icc memfd failed: {err}");
                            queue_image_description_info_done(info, state);
                            resource.post_error(
                                wp_image_description_v1::Error::NotReady,
                                "failed to export ICC profile",
                            );
                        }
                    }
                } else {
                    send_image_description_info(info, &data.description, state);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> for DesktopState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
        _request: wp_image_description_info_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        {}
    }
}

impl Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OrphanOutputColorManagement>
    for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wp_color_management_output_v1::WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        _data: &OrphanOutputColorManagement,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_color_management_output_v1::Request::Destroy = request {}
    }
}

impl Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OutputColorManagement>
    for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &wp_color_management_output_v1::WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        output_mgmt: &OutputColorManagement,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!("output {} {request:?}", client_trace_prefix(client, dh));
        match request {
            wp_color_management_output_v1::Request::Destroy => {}
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                wp_color_trace!("output get_image_description");
                let output_id = state.output_id_for_space_output(&output_mgmt.output);
                let description = output_id
                    .map(|output_id| resolve_preferred_output_description(state, output_id))
                    .unwrap_or_else(|| state.output_color_description_for(&output_mgmt.output));
                // SDR ICC profiles describe the panel's SDR TRCs and must not
                // replace a live output's parametric BT.2020/PQ description.
                let icc_profile =
                    (!crate::core::color::is_hdr_client_preferred_transfer(description.transfer))
                        .then(|| state.output_icc_profile_for(&output_mgmt.output))
                        .flatten();
                finish_output_image_description(
                    state,
                    data_init,
                    image_description,
                    description,
                    icc_profile,
                );
            }
            _ => {}
        }
    }
}

impl
    Dispatch<
        wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        SurfaceColorFeedback,
    > for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        feedback: &SurfaceColorFeedback,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!(
            "surface feedback {} {request:?}",
            client_trace_prefix(client, dh)
        );
        match request {
            wp_color_management_surface_feedback_v1::Request::Destroy => {
                wp_color_trace!("surface feedback destroy");
                state
                    .color_management_state
                    .surface_feedbacks
                    .retain(|f| f.id() != resource.id());
                if feedback.is_inert() {
                    resource.post_error(
                        wp_color_management_surface_feedback_v1::Error::Inert,
                        "wl_surface destroyed",
                    );
                }
            }
            wp_color_management_surface_feedback_v1::Request::GetPreferred {
                image_description,
            }
            | wp_color_management_surface_feedback_v1::Request::GetPreferredParametric {
                image_description,
            } => {
                wp_color_trace!("surface feedback get_preferred");
                if feedback.is_inert() {
                    init_failed_image_description(
                        data_init,
                        image_description,
                        "wl_surface destroyed",
                    );
                    resource.post_error(
                        wp_color_management_surface_feedback_v1::Error::Inert,
                        "wl_surface destroyed",
                    );
                    return;
                }
                let output_id = state.preferred_output_id_for_surface(&feedback.surface);
                let description = resolve_preferred_output_description(state, output_id);
                let icc_profile =
                    (!crate::core::color::is_hdr_client_preferred_transfer(description.transfer))
                        .then(|| {
                            state
                                .outputs
                                .get(&output_id)
                                .and_then(|output| output.icc_profile.clone())
                        })
                        .flatten();
                wp_color_trace!(
                    "surface feedback get_preferred: output={output_id:?} transfer={:?} primaries={:?}",
                    description.transfer,
                    description.primaries
                );
                finish_preferred_output_image_description(
                    state,
                    data_init,
                    image_description,
                    output_id,
                    description,
                    icc_profile,
                );
            }
            _ => {
                wp_color_trace!("surface feedback other request");
                if feedback.is_inert() {
                    resource.post_error(
                        wp_color_management_surface_feedback_v1::Error::Inert,
                        "wl_surface destroyed",
                    );
                }
            }
        }
    }
}

impl Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, SurfaceColorManagement>
    for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        surface_mgmt: &SurfaceColorManagement,
        dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        wp_color_trace!("surface {} {request:?}", client_trace_prefix(client, dh));
        if surface_mgmt.is_inert() {
            wp_color_trace!("surface request rejected: inert surface");
            resource.post_error(
                wp_color_management_surface_v1::Error::Inert,
                "wl_surface destroyed",
            );
            return;
        }

        match request {
            wp_color_management_surface_v1::Request::Destroy => {
                wp_color_trace!("surface destroy");
                apply_surface_description(
                    state,
                    &surface_mgmt.surface,
                    None,
                    RenderingIntent::Perceptual,
                );
            }
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                wp_color_trace!("surface set_image_description: intent={render_intent:?}");
                let Some(intent) = intent_from_wire(render_intent) else {
                    resource.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "unsupported rendering intent",
                    );
                    return;
                };
                let data = image_description.data::<ImageDescriptionData>();
                let Some(data) = data else {
                    resource.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "invalid image description",
                    );
                    return;
                };
                if !data.ready {
                    wp_color_trace!(
                        "surface set_image_description rejected: description not ready"
                    );
                    resource.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "image description not ready",
                    );
                    return;
                }
                flog_warn!(
                    "wp color pending: surface={:?} transfer={:?} primaries={:?}",
                    surface_mgmt.surface.id(),
                    data.description.transfer,
                    data.description.primaries
                );
                wp_color_trace!(
                    "surface pending description accepted: surface={:?} transfer={:?} ready={}",
                    surface_mgmt.surface.id(),
                    data.description.transfer,
                    data.ready
                );
                apply_surface_description(
                    state,
                    &surface_mgmt.surface,
                    Some(data.description),
                    intent,
                );
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                wp_color_trace!("surface unset_image_description");
                apply_surface_description(
                    state,
                    &surface_mgmt.surface,
                    None,
                    RenderingIntent::Perceptual,
                );
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: backend::ClientId,
        _resource: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        surface_mgmt: &SurfaceColorManagement,
    ) {
        state
            .color_management_state
            .surface_objects
            .remove(&surface_mgmt.surface.id());
        wp_color_trace!(
            "surface management destroyed: surface={:?}",
            surface_mgmt.surface.id()
        );
    }
}

impl
    Dispatch<
        wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        OrphanSurfaceColorManagement,
    > for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        _data: &OrphanSurfaceColorManagement,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_color_management_surface_v1::Request::Destroy = request {}
    }
}

#[cfg(test)]
mod tests {
    use super::{is_cursor_executable_name, primary_luminance_wire_values};
    use crate::core::color::ColorDescription;
    use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::TransferFunction as WpTransferFunction;

    #[test]
    fn cursor_executable_name_filter_is_strict() {
        assert!(is_cursor_executable_name("cursor"));
        assert!(is_cursor_executable_name("cursor-bin"));
        assert!(!is_cursor_executable_name("Cursor"));
        assert!(!is_cursor_executable_name("cursor.exe"));
        assert!(!is_cursor_executable_name("code"));
    }

    #[test]
    fn luminance_wire_values_use_protocol_units() {
        let (min_lum, max_lum, reference_lum) =
            primary_luminance_wire_values(&ColorDescription::SRGB);
        assert_eq!(min_lum, 2000);
        assert_eq!(max_lum, 80);
        assert_eq!(reference_lum, 80);
    }

    #[test]
    fn pq_luminance_wire_values_use_hdr_defaults() {
        let description = ColorDescription::bt2020_pq_hdr(600.0, 400.0);
        let (min_lum, max_lum, reference_lum) = primary_luminance_wire_values(&description);
        assert_eq!(min_lum, 50);
        assert_eq!(max_lum, 10_000);
        assert_eq!(reference_lum, 203);
        assert_eq!(super::target_luminance_wire_values(&description), (50, 600));
    }

    #[test]
    fn primaries_from_wire_uses_protocol_scale_only() {
        let ch = super::primaries_from_wire(
            648450, 330840, 230250, 701480, 155890, 66030, 345700, 358540,
        )
        .expect("1e6 scale");
        assert!((ch.r[0] - 0.64845).abs() < 0.001);

        // A ten-times-too-small encoding is not a plausible RGB primary set.
        assert!(
            super::primaries_from_wire(30422, 27028, 23002, 44484, 39810, 26103, 31270, 32900)
                .is_none()
        );
    }

    #[test]
    fn chromium_style_custom_p3_primaries_survive_wire_decode() {
        let ch = super::primaries_from_wire(
            680_000, 320_000, 265_000, 690_000, 150_000, 60_000, 312_700, 329_000,
        )
        .expect("Display P3 chromaticities");
        let description = ColorDescription {
            primaries: crate::core::color::ColorPrimaries::Custom(ch),
            ..ColorDescription::DISPLAY_P3_SRGB
        };
        let render = crate::core::color::SurfaceColorRenderState::for_description(
            description,
            crate::core::color::RenderingIntent::Perceptual,
        );
        assert_ne!(
            render.client_to_scene,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
    }

    #[test]
    fn primaries_plausible_rejects_tiny_coordinates() {
        use crate::core::color::PrimariesChromaticity;
        let tiny = PrimariesChromaticity {
            r: [0.0304229, 0.0270282],
            g: [0.0230029, 0.044484],
            b: [0.03981, 0.0261034],
            w: [0.03127, 0.0329],
        };
        assert!(!crate::core::color::primaries_plausible(&tiny));
        assert!(crate::core::color::primaries_plausible(
            &PrimariesChromaticity::SRGB
        ));
    }

    #[test]
    fn windows_scrgb_wire_luminances_keep_unit_white_at_eighty_nits() {
        let (_min, max, reference) =
            super::primary_luminance_wire_values(&ColorDescription::WINDOWS_SCRGB);
        assert_eq!(reference, 80);
        assert_eq!(max, 10_000);
        assert!(ColorDescription::WINDOWS_SCRGB.is_windows_scrgb());
    }

    #[test]
    fn output_without_icc_uses_canonical_sdr_advertisement_flag() {
        let data = super::ImageDescriptionData {
            identity: 1,
            description: ColorDescription::SRGB,
            ready: true,
            allows_information: true,
            icc_profile: None,
            advertise_as_canonical_sdr: true,
        };
        assert!(data.advertise_as_canonical_sdr);
        assert!(data.icc_profile.is_none());
    }

    #[test]
    fn non_srgb_output_without_icc_is_not_forced_to_canonical_sdr() {
        let description = ColorDescription::DISPLAY_P3_SRGB;
        let advertised = super::output_advertised_description(description);
        assert_eq!(
            advertised.primaries,
            crate::core::color::ColorPrimaries::DisplayP3
        );
        assert_eq!(
            advertised.transfer,
            crate::core::color::TransferFunction::Srgb
        );
    }

    #[test]
    fn hdr_preferred_ext_srgb_is_not_collapsed_to_sdr_srgb() {
        let advertised =
            super::output_advertised_description(ColorDescription::DISPLAY_P3_SRGB_HDR);
        assert_eq!(
            advertised.transfer,
            crate::core::color::TransferFunction::SrgbHdr
        );
        assert_eq!(
            advertised.primaries,
            crate::core::color::ColorPrimaries::DisplayP3
        );
    }

    #[test]
    fn parametric_ext_linear_is_preserved_for_client_surfaces() {
        let creator = super::ParametricCreatorInner {
            primaries: Some(crate::core::color::ColorPrimaries::Srgb),
            tf: Some(super::ParametricTransfer::Named(
                WpTransferFunction::ExtLinear,
            )),
            ..Default::default()
        };
        let description = super::build_description_from_params(&creator).expect("valid params");
        assert_eq!(
            description.transfer,
            crate::core::color::TransferFunction::Linear
        );
        assert!(!description.is_windows_scrgb());
    }

    #[test]
    fn parametric_ext_linear_eighty_nit_unit_is_windows_scrgb() {
        let creator = super::ParametricCreatorInner {
            primaries: Some(crate::core::color::ColorPrimaries::Srgb),
            tf: Some(super::ParametricTransfer::Named(
                WpTransferFunction::ExtLinear,
            )),
            reference_luminance_nits: Some(80),
            max_luminance_nits: Some(10_000),
            ..Default::default()
        };
        let description = super::build_description_from_params(&creator).expect("valid params");
        assert!(description.is_windows_scrgb());
        assert_eq!(
            description.linear_to_scene_scale(),
            80.0 / crate::core::color::HDR_REFERENCE_WHITE_NITS
        );
    }

    #[test]
    fn parametric_srgb_tf_maps_to_internal_srgb() {
        let creator = super::ParametricCreatorInner {
            primaries: Some(crate::core::color::ColorPrimaries::DisplayP3),
            tf: Some(super::ParametricTransfer::Named(WpTransferFunction::Srgb)),
            ..Default::default()
        };
        let description = super::build_description_from_params(&creator).expect("valid params");
        assert_eq!(
            description.primaries,
            crate::core::color::ColorPrimaries::DisplayP3
        );
        assert_eq!(
            description.transfer,
            crate::core::color::TransferFunction::Srgb
        );
    }

    #[test]
    fn parametric_ext_srgb_maps_to_srgb_hdr() {
        let creator = super::ParametricCreatorInner {
            primaries: Some(crate::core::color::ColorPrimaries::DisplayP3),
            tf: Some(super::ParametricTransfer::Named(
                WpTransferFunction::ExtSrgb,
            )),
            ..Default::default()
        };
        let description = super::build_description_from_params(&creator).expect("valid params");
        assert_eq!(
            description.primaries,
            crate::core::color::ColorPrimaries::DisplayP3
        );
        assert_eq!(
            description.transfer,
            crate::core::color::TransferFunction::SrgbHdr
        );
        assert!(!description.is_windows_scrgb());
        assert_eq!(description.linear_to_scene_scale(), 1.0);
    }

    #[test]
    fn parametric_pq_preserves_absolute_luminance_units_and_metadata() {
        let creator = super::ParametricCreatorInner {
            primaries: Some(crate::core::color::ColorPrimaries::Bt2020),
            tf: Some(super::ParametricTransfer::Named(
                WpTransferFunction::St2084Pq,
            )),
            min_luminance_x10000: Some(50),
            max_luminance_nits: Some(10_000),
            reference_luminance_nits: Some(203),
            mastering_max_luminance_nits: Some(1_000),
            max_cll_nits: Some(1_000),
            max_fall_nits: Some(400),
            ..Default::default()
        };
        let description = super::build_description_from_params(&creator).expect("valid PQ params");
        assert_eq!(
            description.transfer,
            crate::core::color::TransferFunction::St2084Pq
        );
        assert_eq!(description.reference_white_nits, 203.0);
        assert_eq!(description.max_luminance_nits, 1_000.0);
        assert_eq!(description.max_cll_nits, Some(1_000.0));
        assert_eq!(description.max_fall_nits, Some(400.0));
    }
}

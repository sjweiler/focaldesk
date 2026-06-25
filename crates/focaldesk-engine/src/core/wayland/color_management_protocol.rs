//! [`wp_color_management_v1`](https://wayland.app/protocols/color-management-v1) server.

use crate::core::color::{
    ColorDescription, ColorPrimaries, PrimariesChromaticity, RenderingIntent, SurfaceColorState,
    TransferFunction as CoreTransferFunction,
};
use crate::core::icc::{self, read_icc_from_fd, parse_icc_profile};
use crate::core::desktop::is_browser_like;
use crate::core::desktop::DesktopState;
use crate::core::wayland::client::ClientState;
use focaldesk_logging::flog;
use smithay::output::Output;
use smithay::wayland::compositor::{add_destruction_hook, with_states};
use std::collections::HashSet;
use std::sync::Mutex;
use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1, wp_color_management_surface_feedback_v1,
    wp_color_management_surface_v1, wp_color_manager_v1, wp_image_description_creator_icc_v1,
    wp_image_description_creator_params_v1, wp_image_description_info_v1, wp_image_description_v1,
};
use wayland_server::{
    backend, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use wayland_server::protocol::wl_surface::WlSurface;
use std::os::fd::AsFd;

fn wp_color_trace(msg: impl AsRef<str>) {
    use std::io::Write;

    let line = format!("[wp-color] {}", msg.as_ref());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/focaldesk-wp-color.trace")
    {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
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

fn client_exe_basename(credentials: &crate::core::wayland::client::ClientCredentials) -> Option<String> {
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

fn client_is_browser_like(client: &Client) -> bool {
    let Some(client_state) = client.get_data::<ClientState>() else {
        return false;
    };
    let Some(credentials) = client_state.credentials else {
        return false;
    };

    let Some(exe_name) = client_exe_basename(&credentials) else {
        return false;
    };

    is_browser_like(exe_name.as_ref())
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

fn preferred_output_description_identity(state: &mut DesktopState) -> u64 {
    let has_icc = state
        .outputs
        .get(&state.primary_output)
        .and_then(|output| output.icc_profile.as_ref())
        .is_some_and(|icc| !icc.is_empty());
    if has_icc {
        state.color_management_state.next_identity()
    } else {
        state.color_management_state.canonical_sdr_identity()
    }
}

/// Notify all `wp_color_management_surface_feedback_v1` objects after an output profile change.
pub fn notify_preferred_color_changed(state: &mut DesktopState) {
    let identity = state.color_management_state.next_description_identity();
    for feedback in &state.color_management_state.surface_feedbacks {
        send_surface_feedback_preferred_changed(feedback, identity);
    }
}

#[derive(Default)]
pub struct ColorManagementState {
    pub surface_objects: HashSet<backend::ObjectId>,
    pub surface_feedbacks: Vec<wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1>,
    next_description_identity: u64,
    canonical_sdr_identity: Option<u64>,
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
            >
            + 'static,
    {
        wp_color_trace("binding wp_color_management_v1 global");
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
    min_luminance_mcd: Option<u32>,
    max_luminance_mcd: Option<u32>,
    reference_luminance_mcd: Option<u32>,
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
    surface: WlSurface,
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

fn send_manager_advertisement(manager: &wp_color_manager_v1::WpColorManagerV1) {
    use wp_color_manager_v1::{Feature, Primaries, RenderIntent, TransferFunction};

    manager.supported_intent(RenderIntent::Perceptual);
    manager.supported_intent(RenderIntent::Relative);

    manager.supported_feature(Feature::IccV2V4);
    manager.supported_feature(Feature::Parametric);
    manager.supported_feature(Feature::SetPrimaries);
    manager.supported_feature(Feature::SetTfPower);
    manager.supported_feature(Feature::SetLuminances);

    manager.supported_tf_named(TransferFunction::Bt1886);
    manager.supported_tf_named(TransferFunction::Gamma22);
    manager.supported_tf_named(TransferFunction::ExtLinear);
    manager.supported_tf_named(TransferFunction::CompoundPower24);

    manager.supported_primaries_named(Primaries::Srgb);
    manager.supported_primaries_named(Primaries::DisplayP3);
    manager.supported_primaries_named(Primaries::Bt2020);
    manager.done();
    wp_color_trace("manager advertisement sent");
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
    wp_color_trace(format!("image description init failed: {msg}"));
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
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = state.color_management_state.next_identity();
    wp_color_trace(format!(
        "image description finished: id={identity} ready=true allow_info={allows_information} canonical_sdr={advertise_as_canonical_sdr} desc={:?}",
        description
    ));
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
    wp_color_trace(format!(
        "canonical output image description finished: id={identity} desc={description:?}"
    ));
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
    if icc_profile.as_ref().is_some_and(|icc| !icc.is_empty()) {
        finish_image_description(state, data_init, id, description, true, icc_profile, false);
    } else {
        // KMS scanout is sRGB; advertise canonical sRGB to clients (Chrome/KWin path).
        let _ = description;
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
    // ICC sRGB defaults: min 0.2 cd/m² (wire ×10000), max/ref unscaled cd/m².
    let min_lum = (0.2_f32 * 10_000.0).round() as u32;
    let max_lum = description.max_luminance_nits.round().max(1.0) as u32;
    let reference_lum = description.reference_white_nits.round().max(1.0) as u32;
    (min_lum, max_lum, reference_lum)
}

fn chromaticity_is_valid(ch: &PrimariesChromaticity) -> bool {
    let in_range = |xy: [f32; 2]| xy[0].is_finite() && xy[1].is_finite() && xy[0] >= 0.0 && xy[0] <= 1.0 && xy[1] >= 0.0 && xy[1] <= 1.0;
    in_range(ch.r) && in_range(ch.g) && in_range(ch.b) && in_range(ch.w)
}

fn send_canonical_sdr_image_description_info(
    info: wp_image_description_info_v1::WpImageDescriptionInfoV1,
    state: &mut DesktopState,
) {
    use wp_color_manager_v1::{Primaries, TransferFunction};

    info.primaries_named(Primaries::Srgb);
    info.tf_named(TransferFunction::Bt1886);
    let (min_lum, max_lum, reference_lum) = primary_luminance_wire_values(&ColorDescription::SRGB);
    info.luminances(min_lum, max_lum, reference_lum);
    if info.version() >= 2 {
        info.target_luminance(min_lum, max_lum);
    }
    queue_image_description_info_done(info, state);
}

fn send_image_description_info(
    info: wp_image_description_info_v1::WpImageDescriptionInfoV1,
    description: &ColorDescription,
    state: &mut DesktopState,
) {
    use wp_color_manager_v1::{Primaries, TransferFunction};

    let use_custom = match description.primaries {
        ColorPrimaries::Custom(ch) => chromaticity_is_valid(&ch),
        _ => false,
    };

    match description.primaries {
        ColorPrimaries::Srgb if !use_custom => info.primaries_named(Primaries::Srgb),
        ColorPrimaries::DisplayP3 if !use_custom => info.primaries_named(Primaries::DisplayP3),
        ColorPrimaries::Bt2020 if !use_custom => info.primaries_named(Primaries::Bt2020),
        ColorPrimaries::Custom(ch) if use_custom => {
            info.primaries(
                (ch.r[0] * 100_000.0).round() as i32,
                (ch.r[1] * 100_000.0).round() as i32,
                (ch.g[0] * 100_000.0).round() as i32,
                (ch.g[1] * 100_000.0).round() as i32,
                (ch.b[0] * 100_000.0).round() as i32,
                (ch.b[1] * 100_000.0).round() as i32,
                (ch.w[0] * 100_000.0).round() as i32,
                (ch.w[1] * 100_000.0).round() as i32,
            );
        }
        _ => {
            wp_color_trace(
                "get_information: invalid output primaries, falling back to sRGB advertisement",
            );
            info.primaries_named(Primaries::Srgb);
        }
    }

    let tf_named = match description.transfer {
        CoreTransferFunction::Srgb | CoreTransferFunction::Bt1886 => TransferFunction::Bt1886,
        CoreTransferFunction::Gamma22 => TransferFunction::Gamma22,
        CoreTransferFunction::Linear => TransferFunction::ExtLinear,
    };
    info.tf_named(tf_named);

    let (min_lum, max_lum, reference_lum) = primary_luminance_wire_values(description);
    info.luminances(min_lum, max_lum, reference_lum);
    if info.version() >= 2 {
        // SDR: target volume equals primary volume — omit target_primaries.
        info.target_luminance(min_lum, max_lum);
    }
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
            wp_color_manager_v1::TransferFunction::CompoundPower24 => CoreTransferFunction::Srgb,
            wp_color_manager_v1::TransferFunction::ExtLinear => CoreTransferFunction::Linear,
            _ => return Err("unsupported named transfer function".into()),
        },
        ParametricTransfer::Power(exp) if (exp - 2.4).abs() <= 0.0001 => CoreTransferFunction::Srgb,
        ParametricTransfer::Power(exp) if (exp - 2.2).abs() <= 0.0001 => {
            CoreTransferFunction::Gamma22
        }
        ParametricTransfer::Power(_) => return Err("unsupported power transfer function".into()),
    };

    let ref_nits = creator
        .reference_luminance_mcd
        .map(|mcd| mcd as f32 / 10_000.0)
        .unwrap_or(80.0);
    let max_nits = creator
        .max_luminance_mcd
        .map(|mcd| mcd as f32 / 10_000.0)
        .unwrap_or(ref_nits);
    let _min_nits = creator
        .min_luminance_mcd
        .map(|mcd| mcd as f32 / 10_000.0)
        .unwrap_or(0.0);

    Ok(ColorDescription {
        primaries,
        transfer: mapped_tf,
        reference_white_nits: ref_nits,
        max_luminance_nits: max_nits,
        max_cll_nits: None,
        max_fall_nits: None,
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
    wp_color_trace(format!(
        "apply surface description: surface={:?} desc={:?} intent={intent:?}",
        surface.id(),
        description
    ));
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
        wp_color_trace(format!(
            "wp_color_manager_v1 bound {}",
            client_trace_prefix(client, _handle)
        ));
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
        wp_color_trace(format!(
            "manager {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_color_manager_v1::Request::Destroy => {}
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                wp_color_trace(format!(
                    "manager get_surface: {} surface={:?}",
                    client_trace_prefix(client, dh),
                    surface.id()
                ));
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
                wp_color_trace(format!(
                    "manager get_output: {} output={:?}",
                    client_trace_prefix(client, dh),
                    output.id()
                ));
                let Some(output_handle) = Output::from_resource(&output) else {
                    wp_color_trace(format!(
                        "manager get_output rejected: {} invalid wl_output={:?}",
                        client_trace_prefix(client, dh),
                        output.id()
                    ));
                    data_init.init(id, OrphanOutputColorManagement);
                    resource.post_error(
                        wp_color_manager_v1::Error::UnsupportedFeature,
                        "invalid wl_output",
                    );
                    return;
                };
                data_init.init(id, OutputColorManagement {
                    output: output_handle,
                });
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                wp_color_trace(format!(
                    "manager get_surface_feedback: {} surface={:?}",
                    client_trace_prefix(client, dh),
                    surface.id()
                ));
                let feedback = data_init.init(id, SurfaceColorFeedback::new(surface));
                let identity = preferred_output_description_identity(state);
                send_surface_feedback_preferred_changed(&feedback, identity);
                state
                    .color_management_state
                    .surface_feedbacks
                    .push(feedback);
            }
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                wp_color_trace(format!(
                    "manager create_parametric_creator: {}",
                    client_trace_prefix(client, dh)
                ));
                data_init.init(obj, ParametricCreatorState::default());
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                wp_color_trace(format!(
                    "manager create_icc_creator: {}",
                    client_trace_prefix(client, dh)
                ));
                data_init.init(obj, IccCreatorState::default());
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                wp_color_trace(format!(
                    "manager create_windows_scrgb rejected: {}",
                    client_trace_prefix(client, dh)
                ));
                init_failed_image_description(
                    data_init,
                    image_description,
                    "Windows-scRGB is not supported",
                );
                resource.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "request not supported",
                );
            }
            wp_color_manager_v1::Request::GetImageDescription {
                image_description,
                reference: _,
            } => {
                wp_color_trace(format!(
                    "manager get_image_description rejected: {}",
                    client_trace_prefix(client, dh)
                ));
                init_failed_image_description(
                    data_init,
                    image_description,
                    "image description references are not supported",
                );
            }
            other => {
                wp_color_trace(format!("manager unhandled: {other:?}"));
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
        wp_color_trace(format!(
            "icc creator {} {request:?}",
            client_trace_prefix(client, dh)
        ));
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
                        );
                    }
                    Err(err) => {
                        wp_color_trace(format!("ICC parse failed: {err:?}"));
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
        wp_color_trace(format!(
            "parametric creator {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_image_description_creator_params_v1::Request::SetTfNamed { tf } => {
                wp_color_trace("parametric creator set_tf_named");
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
                wp_color_trace(format!("parametric creator set_tf_power: eexp={eexp}"));
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
                wp_color_trace(format!(
                    "parametric creator set_primaries_named: primaries={primaries:?}"
                ));
                let WEnum::Value(primaries) = primaries else {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::InvalidPrimariesNamed,
                        "invalid primaries",
                    );
                    return;
                };
                let Some(mapped) = ColorPrimaries::from_wp_named(primaries) else {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::InvalidPrimariesNamed,
                        "unsupported primaries",
                    );
                    return;
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
                wp_color_trace("parametric creator create");
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
                let scale = 100_000.0f32;
                let ch = PrimariesChromaticity {
                    r: [r_x as f32 / scale, r_y as f32 / scale],
                    g: [g_x as f32 / scale, g_y as f32 / scale],
                    b: [b_x as f32 / scale, b_y as f32 / scale],
                    w: [w_x as f32 / scale, w_y as f32 / scale],
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
                inner.primaries = Some(ColorPrimaries::Custom(ch));
            }
            wp_image_description_creator_params_v1::Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                let mut inner = creator.inner.lock().unwrap();
                if inner.reference_luminance_mcd.is_some() {
                    post_creator_error(
                        resource,
                        wp_image_description_creator_params_v1::Error::AlreadySet,
                        "luminances already set",
                    );
                    return;
                }
                inner.min_luminance_mcd = Some(min_lum);
                inner.max_luminance_mcd = Some(max_lum);
                inner.reference_luminance_mcd = Some(reference_lum);
            }
            wp_image_description_creator_params_v1::Request::SetMasteringDisplayPrimaries {
                ..
            }
            | wp_image_description_creator_params_v1::Request::SetMasteringLuminance { .. }
            | wp_image_description_creator_params_v1::Request::SetMaxCll { .. }
            | wp_image_description_creator_params_v1::Request::SetMaxFall { .. } => {
                post_creator_error(
                    resource,
                    wp_image_description_creator_params_v1::Error::UnsupportedFeature,
                    "request not supported",
                );
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
        wp_color_trace(format!(
            "image description {} {request:?}",
            client_trace_prefix(client, dh)
        ));
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
                if let Some(icc) = &data.icc_profile {
                    match icc::memfd_from_bytes(icc) {
                        Ok(fd) => {
                            info.icc_file(fd.as_fd(), icc.len() as u32);
                            queue_image_description_info_done(info, state);
                        }
                        Err(err) => {
                            wp_color_trace(format!("icc memfd failed: {err}"));
                            queue_image_description_info_done(info, state);
                            resource.post_error(
                                wp_image_description_v1::Error::NotReady,
                                "failed to export ICC profile",
                            );
                        }
                    }
                } else if data.advertise_as_canonical_sdr {
                    send_canonical_sdr_image_description_info(info, state);
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

impl
    Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OrphanOutputColorManagement>
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
        wp_color_trace(format!(
            "output {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_color_management_output_v1::Request::Destroy => {}
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                wp_color_trace("output get_image_description");
                let description = state.output_color_description_for(&output_mgmt.output);
                let icc_profile = state.output_icc_profile_for(&output_mgmt.output);
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
        wp_color_trace(format!(
            "surface feedback {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_color_management_surface_feedback_v1::Request::Destroy => {
                wp_color_trace("surface feedback destroy");
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
                wp_color_trace("surface feedback get_preferred");
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
                let description = state.output_color_description(state.primary_output);
                let icc_profile = state
                    .outputs
                    .get(&state.primary_output)
                    .and_then(|output| output.icc_profile.clone());
                finish_output_image_description(
                    state,
                    data_init,
                    image_description,
                    description,
                    icc_profile,
                );
            }
            _ => {
                wp_color_trace("surface feedback other request");
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
        wp_color_trace(format!(
            "surface {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        if surface_mgmt.is_inert() {
            wp_color_trace("surface request rejected: inert surface");
            resource.post_error(
                wp_color_management_surface_v1::Error::Inert,
                "wl_surface destroyed",
            );
            return;
        }

        match request {
            wp_color_management_surface_v1::Request::Destroy => {
                wp_color_trace("surface destroy");
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
                wp_color_trace(format!(
                    "surface set_image_description: intent={render_intent:?}"
                ));
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
                    wp_color_trace("surface set_image_description rejected: description not ready");
                    resource.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "image description not ready",
                    );
                    return;
                }
                flog(format!(
                    "wp color pending: surface={:?} transfer={:?}",
                    surface_mgmt.surface.id(),
                    data.description.transfer
                ));
                wp_color_trace(format!(
                    "surface pending description accepted: surface={:?} transfer={:?} ready={}",
                    surface_mgmt.surface.id(),
                    data.description.transfer,
                    data.ready
                ));
                apply_surface_description(
                    state,
                    &surface_mgmt.surface,
                    Some(data.description),
                    intent,
                );
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                wp_color_trace("surface unset_image_description");
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
        wp_color_trace(format!(
            "surface management destroyed: surface={:?}",
            surface_mgmt.surface.id()
        ));
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
}

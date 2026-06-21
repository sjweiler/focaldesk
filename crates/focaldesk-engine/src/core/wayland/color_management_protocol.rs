//! [`wp_color_management_v1`](https://wayland.app/protocols/color-management-v1) server.

use crate::core::color::{ColorDescription, RenderingIntent, SurfaceColorState, TransferFunction};
use crate::core::desktop::DesktopState;
use crate::core::desktop::is_browser_like;
use crate::core::wayland::client::ClientState;
use focaldesk_logging::{flog, flog_critical};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
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

fn client_is_cursor(client: &Client) -> bool {
    let Some(client_state) = client.get_data::<ClientState>() else {
        return false;
    };
    let Some(credentials) = client_state.credentials else {
        return false;
    };

    let exe_path = std::fs::read_link(format!("/proc/{}/exe", credentials.pid)).ok();
    let Some(exe_name) = exe_path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy())
    else {
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

    let exe_path = std::fs::read_link(format!("/proc/{}/exe", credentials.pid)).ok();
    let Some(exe_name) = exe_path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy())
    else {
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

#[derive(Default)]
pub struct ColorManagementState {
    pub surface_objects: HashSet<backend::ObjectId>,
    next_description_identity: u64,
    canonical_sdr_identity: Option<u64>,
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
            > + Dispatch<wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1, ()>
            + 'static,
    {
        wp_color_trace("binding wp_color_management_v1 global");
        display.create_global::<D, wp_color_manager_v1::WpColorManagerV1, _>(2, ());
    }

    fn next_identity(&mut self) -> u64 {
        self.next_description_identity = self.next_description_identity.wrapping_add(1).max(1);
        self.next_description_identity
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
}

#[derive(Default)]
pub struct ParametricCreatorState {
    inner: Mutex<ParametricCreatorInner>,
}

#[derive(Debug, Default)]
struct ParametricCreatorInner {
    tf: Option<ParametricTransfer>,
    primaries: Option<wp_color_manager_v1::Primaries>,
}

#[derive(Debug, Clone, Copy)]
enum ParametricTransfer {
    Named(wp_color_manager_v1::TransferFunction),
    Power(f32),
}

pub struct OutputColorManagement;

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

    manager.supported_feature(Feature::Parametric);
    manager.supported_feature(Feature::SetPrimaries);
    manager.supported_feature(Feature::SetTfPower);
    manager.supported_feature(Feature::SetLuminances);

    manager.supported_tf_named(TransferFunction::Bt1886);
    manager.supported_tf_named(TransferFunction::Gamma22);
    manager.supported_tf_named(TransferFunction::ExtLinear);
    manager.supported_tf_named(TransferFunction::CompoundPower24);

    manager.supported_primaries_named(Primaries::Srgb);
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
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = state.color_management_state.next_identity();
    wp_color_trace(format!(
        "image description finished: id={identity} ready=true allow_info={allows_information} desc={:?}",
        description
    ));
    let image = data_init.init(
        id,
        ImageDescriptionData {
            identity,
            description,
            ready: true,
            allows_information,
        },
    );
    send_image_description_ready(&image, identity);
}

fn finish_canonical_sdr_image_description<D>(
    state: &mut DesktopState,
    data_init: &mut DataInit<'_, D>,
    id: New<wp_image_description_v1::WpImageDescriptionV1>,
) where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let identity = state.color_management_state.canonical_sdr_identity();
    wp_color_trace(format!(
        "canonical sdr image description finished: id={identity}"
    ));
    let image = data_init.init(
        id,
        ImageDescriptionData {
            identity,
            description: ColorDescription::SRGB,
            ready: true,
            allows_information: true,
        },
    );
    send_image_description_ready(&image, identity);
}

fn init_inert_image_description_info<D>(
    data_init: &mut DataInit<'_, D>,
    information: New<wp_image_description_info_v1::WpImageDescriptionInfoV1>,
) where
    D: Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> + 'static,
{
    let info = data_init.init(information, ());
    info.done();
}

fn send_sdr_image_description_info(info: &wp_image_description_info_v1::WpImageDescriptionInfoV1) {
    use wp_color_manager_v1::{Primaries, TransferFunction};

    info.primaries_named(Primaries::Srgb);
    info.tf_named(TransferFunction::Bt1886);
    // BT.1886 default luminances from the protocol appendix.
    info.luminances(100, 100, 100);
    info.done();
}

fn build_description_from_params(
    creator: &ParametricCreatorInner,
) -> Result<ColorDescription, String> {
    let primaries = creator
        .primaries
        .ok_or_else(|| "missing primaries".to_string())?;
    if primaries != wp_color_manager_v1::Primaries::Srgb {
        return Err("unsupported primaries".into());
    }

    let transfer = creator
        .tf
        .ok_or_else(|| "missing transfer function".to_string())?;

    let mapped = match transfer {
        ParametricTransfer::Named(tf) => match tf {
            wp_color_manager_v1::TransferFunction::Bt1886
            | wp_color_manager_v1::TransferFunction::Gamma22
            | wp_color_manager_v1::TransferFunction::CompoundPower24 => TransferFunction::Srgb,
            wp_color_manager_v1::TransferFunction::ExtLinear => TransferFunction::Linear,
            _ => return Err("unsupported named transfer function".into()),
        },
        ParametricTransfer::Power(exp) if (exp - 2.4).abs() <= 0.0001 => TransferFunction::Srgb,
        ParametricTransfer::Power(_) => return Err("unsupported power transfer function".into()),
    };

    Ok(match mapped {
        TransferFunction::Srgb => ColorDescription::SRGB,
        TransferFunction::Linear => ColorDescription::LINEAR_SRGB,
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
        !client_is_cursor(&client) && !client_is_browser_like(&client)
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
        flog_critical!(
            "wp color manager request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
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
            wp_color_manager_v1::Request::GetOutput {
                id,
                output: _output,
            } => {
                wp_color_trace(format!(
                    "manager get_output: {} output-requested",
                    client_trace_prefix(client, dh)
                ));
                data_init.init(id, OutputColorManagement);
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                wp_color_trace(format!(
                    "manager get_surface_feedback: {} surface={:?}",
                    client_trace_prefix(client, dh),
                    surface.id()
                ));
                data_init.init(id, SurfaceColorFeedback::new(surface));
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
                    "manager create_icc_creator rejected: {}",
                    client_trace_prefix(client, dh)
                ));
                data_init.init(obj, ());
                resource.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "request not supported",
                );
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

impl Dispatch<wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1, ()>
    for DesktopState
{
    fn request(
        _state: &mut Self,
        client: &Client,
        resource: &wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
        request: wp_image_description_creator_icc_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        flog_critical!(
            "wp color icc creator request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
        wp_color_trace(format!(
            "icc creator {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_image_description_creator_icc_v1::Request::Create { image_description } => {
                init_failed_image_description(
                    data_init,
                    image_description,
                    "ICC image descriptions are not supported",
                );
            }
            _ => {
                resource.post_error(
                    wp_image_description_creator_icc_v1::Error::IncompleteSet,
                    "ICC image descriptions are not supported",
                );
            }
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
        flog_critical!(
            "wp color parametric creator request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
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
                finish_image_description(state, data_init, image_description, description, false);
            }
            wp_image_description_creator_params_v1::Request::SetPrimaries { .. }
            | wp_image_description_creator_params_v1::Request::SetLuminances { .. }
            | wp_image_description_creator_params_v1::Request::SetMasteringDisplayPrimaries {
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
        _state: &mut Self,
        client: &Client,
        resource: &wp_image_description_v1::WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &ImageDescriptionData,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        flog_critical!(
            "wp color image description request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
        wp_color_trace(format!(
            "image description {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_image_description_v1::Request::Destroy => {}
            wp_image_description_v1::Request::GetInformation { information } => {
                if !data.ready {
                    init_inert_image_description_info(data_init, information);
                    resource.post_error(
                        wp_image_description_v1::Error::NotReady,
                        "image description not ready",
                    );
                    return;
                }
                if !data.allows_information {
                    init_inert_image_description_info(data_init, information);
                    resource.post_error(
                        wp_image_description_v1::Error::NoInformation,
                        "image description info unavailable",
                    );
                    return;
                }
                let info = data_init.init(information, ());
                send_sdr_image_description_info(&info);
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

impl Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OutputColorManagement>
    for DesktopState
{
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &wp_color_management_output_v1::WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        _data: &OutputColorManagement,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        flog_critical!(
            "wp color output request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
        wp_color_trace(format!(
            "output {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_color_management_output_v1::Request::Destroy => {}
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                wp_color_trace("output get_image_description");
                finish_canonical_sdr_image_description(state, data_init, image_description);
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
        flog_critical!(
            "wp color surface feedback request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
        wp_color_trace(format!(
            "surface feedback {} {request:?}",
            client_trace_prefix(client, dh)
        ));
        match request {
            wp_color_management_surface_feedback_v1::Request::Destroy => {
                wp_color_trace("surface feedback destroy");
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
                finish_canonical_sdr_image_description(state, data_init, image_description);
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
        flog_critical!(
            "wp color surface request: {} {request:?}",
            client_trace_prefix(client, dh)
        );
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
    use super::is_cursor_executable_name;

    #[test]
    fn cursor_executable_name_filter_is_strict() {
        assert!(is_cursor_executable_name("cursor"));
        assert!(is_cursor_executable_name("cursor-bin"));
        assert!(!is_cursor_executable_name("Cursor"));
        assert!(!is_cursor_executable_name("cursor.exe"));
        assert!(!is_cursor_executable_name("code"));
    }
}

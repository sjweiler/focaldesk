//! Staging [`focaldesk_color_v1`] protocol for client surface color tags.

use crate::core::color::{ColorDescription, SurfaceColorState, TransferFunction};
use crate::core::desktop::DesktopState;
use focaldesk_logging::flog;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{add_destruction_hook, with_states};
use std::collections::HashSet;
use wayland_server::{
    backend, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

mod generated {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]

    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/focaldesk-color-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocols/focaldesk-color-v1.xml");
    }
}

use generated::server::{focaldesk_color_manager_v1, focaldesk_surface_color_v1};

#[derive(Default)]
pub struct ColorTagState {
    tagged_surfaces: HashSet<backend::ObjectId>,
}

impl ColorTagState {
    pub fn bind_global<D>(display: &DisplayHandle)
    where
        D: GlobalDispatch<focaldesk_color_manager_v1::FocaldeskColorManagerV1, ()>
            + Dispatch<focaldesk_color_manager_v1::FocaldeskColorManagerV1, ()>
            + Dispatch<focaldesk_surface_color_v1::FocaldeskSurfaceColorV1, SurfaceColorTag>
            + Dispatch<focaldesk_surface_color_v1::FocaldeskSurfaceColorV1, OrphanSurfaceColorTag>
            + 'static,
    {
        display.create_global::<D, focaldesk_color_manager_v1::FocaldeskColorManagerV1, _>(1, ());
    }
}

/// Placeholder when `get_surface` hits `surface_exists` but Wayland still allocates the `NewId`.
pub struct OrphanSurfaceColorTag;

#[derive(Debug, Clone)]
pub struct SurfaceColorTag {
    surface: WlSurface,
}

impl SurfaceColorTag {
    fn new(surface: WlSurface) -> Self {
        add_destruction_hook::<DesktopState, _>(&surface, |state, surface| {
            state.color_tag_state.tagged_surfaces.remove(&surface.id());
            state.refresh_surface_color(surface);
        });

        Self { surface }
    }

    fn is_inert(&self) -> bool {
        !self.surface.is_alive()
    }
}

fn transfer_from_wire(
    value: WEnum<focaldesk_surface_color_v1::Transfer>,
) -> Option<TransferFunction> {
    use focaldesk_surface_color_v1::Transfer;
    match value {
        WEnum::Value(Transfer::Srgb) => Some(TransferFunction::Srgb),
        WEnum::Value(Transfer::LinearSrgb) => Some(TransferFunction::Linear),
        WEnum::Unknown(_) => None,
    }
}

fn set_pending_description(surface: &WlSurface, description: Option<ColorDescription>) {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceColorState>()
            .pending()
            .description = description;
    });
}

impl GlobalDispatch<focaldesk_color_manager_v1::FocaldeskColorManagerV1, ()> for DesktopState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<focaldesk_color_manager_v1::FocaldeskColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<focaldesk_color_manager_v1::FocaldeskColorManagerV1, ()> for DesktopState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &focaldesk_color_manager_v1::FocaldeskColorManagerV1,
        request: focaldesk_color_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            focaldesk_color_manager_v1::Request::Destroy => {}
            focaldesk_color_manager_v1::Request::GetSurface { id, surface } => {
                if state
                    .color_tag_state
                    .tagged_surfaces
                    .contains(&surface.id())
                {
                    data_init.init(id, OrphanSurfaceColorTag);
                    resource.post_error(
                        focaldesk_color_manager_v1::Error::SurfaceExists,
                        "surface already has a focaldesk_surface_color_v1 object",
                    );
                    return;
                }

                state.color_tag_state.tagged_surfaces.insert(surface.id());
                data_init.init(id, SurfaceColorTag::new(surface));
            }
            _ => {}
        }
    }
}

impl Dispatch<focaldesk_surface_color_v1::FocaldeskSurfaceColorV1, OrphanSurfaceColorTag>
    for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &focaldesk_surface_color_v1::FocaldeskSurfaceColorV1,
        request: focaldesk_surface_color_v1::Request,
        _data: &OrphanSurfaceColorTag,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let focaldesk_surface_color_v1::Request::Destroy = request {}
    }
}

impl Dispatch<focaldesk_surface_color_v1::FocaldeskSurfaceColorV1, SurfaceColorTag>
    for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &focaldesk_surface_color_v1::FocaldeskSurfaceColorV1,
        request: focaldesk_surface_color_v1::Request,
        tag: &SurfaceColorTag,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if tag.is_inert() {
            resource.post_error(
                focaldesk_surface_color_v1::Error::Inert,
                "wl_surface destroyed",
            );
            return;
        }

        match request {
            focaldesk_surface_color_v1::Request::Destroy => {}
            focaldesk_surface_color_v1::Request::SetTransfer { transfer } => {
                let Some(transfer) = transfer_from_wire(transfer) else {
                    return;
                };
                let description = match transfer {
                    TransferFunction::Srgb => ColorDescription::SRGB,
                    TransferFunction::Linear => ColorDescription::LINEAR_SRGB,
                    TransferFunction::Bt1886 => ColorDescription {
                        transfer: TransferFunction::Bt1886,
                        ..ColorDescription::SRGB
                    },
                    TransferFunction::Gamma22 => ColorDescription {
                        transfer: TransferFunction::Gamma22,
                        ..ColorDescription::SRGB
                    },
                };
                set_pending_description(&tag.surface, Some(description));
                flog(format!(
                    "color tag pending: surface={:?} transfer={transfer:?}",
                    tag.surface.id()
                ));
            }
            focaldesk_surface_color_v1::Request::Unset => {
                set_pending_description(&tag.surface, None);
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: backend::ClientId,
        _resource: &focaldesk_surface_color_v1::FocaldeskSurfaceColorV1,
        tag: &SurfaceColorTag,
    ) {
        state
            .color_tag_state
            .tagged_surfaces
            .remove(&tag.surface.id());
    }
}

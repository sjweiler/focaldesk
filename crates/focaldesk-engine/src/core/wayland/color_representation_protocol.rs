//! [`wp_color_representation_v1`](https://wayland.app/protocols/color-representation-v1) server.
//!
//! FocalDesk currently composites RGB buffers. Advertise only the representation
//! combinations the GLES renderer can interpret without an implicit YCbCr or
//! alpha conversion. YCbCr matrices, limited range, chroma siting, straight
//! alpha, and optical premultiplication can be added with their renderer paths.

use crate::core::desktop::DesktopState;
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::wayland::compositor::{
    add_destruction_hook, add_pre_commit_hook, with_states, BufferAssignment, Cacheable,
    SurfaceAttributes,
};
use smithay::wayland::dmabuf::get_dmabuf;
use std::collections::HashMap;
use wayland_protocols::wp::color_representation::v1::server::{
    wp_color_representation_manager_v1, wp_color_representation_surface_v1,
};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{
    backend, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

#[derive(Default)]
pub struct ColorRepresentationState {
    surface_objects: HashMap<
        backend::ObjectId,
        wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
    >,
}

impl ColorRepresentationState {
    pub fn bind_global<D>(display: &DisplayHandle)
    where
        D: GlobalDispatch<wp_color_representation_manager_v1::WpColorRepresentationManagerV1, ()>
            + Dispatch<wp_color_representation_manager_v1::WpColorRepresentationManagerV1, ()>
            + Dispatch<
                wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
                SurfaceRepresentation,
            > + Dispatch<
                wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
                OrphanSurfaceRepresentation,
            > + 'static,
    {
        display.create_global::<D, wp_color_representation_manager_v1::WpColorRepresentationManagerV1, _>(1, ());
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceRepresentationState {
    pub alpha_mode: Option<wp_color_representation_surface_v1::AlphaMode>,
    pub coefficients_and_range: Option<(
        wp_color_representation_surface_v1::Coefficients,
        wp_color_representation_surface_v1::Range,
    )>,
    pub chroma_location: Option<wp_color_representation_surface_v1::ChromaLocation>,
}

impl Cacheable for SurfaceRepresentationState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

pub struct SurfaceRepresentation {
    surface: WlSurface,
}

pub struct OrphanSurfaceRepresentation;

fn is_rgb_fourcc(format: Fourcc) -> bool {
    matches!(
        format,
        Fourcc::Argb8888
            | Fourcc::Xrgb8888
            | Fourcc::Abgr8888
            | Fourcc::Xbgr8888
            | Fourcc::Argb2101010
            | Fourcc::Xrgb2101010
            | Fourcc::Abgr2101010
            | Fourcc::Xbgr2101010
            | Fourcc::Argb16161616f
            | Fourcc::Abgr16161616f
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachedBufferKind {
    None,
    RgbOrShm,
    Dmabuf(Fourcc),
}

fn attached_buffer_kind(surface: &WlSurface) -> AttachedBufferKind {
    with_states(surface, |states| {
        let mut attrs = states.cached_state.get::<SurfaceAttributes>();
        match attrs.pending().buffer.as_ref() {
            Some(BufferAssignment::Removed) => return AttachedBufferKind::None,
            Some(BufferAssignment::NewBuffer(buffer)) => {
                return get_dmabuf(buffer)
                    .ok()
                    .map(|dmabuf| AttachedBufferKind::Dmabuf(dmabuf.format().code))
                    .unwrap_or(AttachedBufferKind::RgbOrShm);
            }
            None => {}
        }
        let Some(BufferAssignment::NewBuffer(buffer)) = attrs.current().buffer.as_ref() else {
            return AttachedBufferKind::None;
        };
        get_dmabuf(buffer)
            .ok()
            .map(|dmabuf| AttachedBufferKind::Dmabuf(dmabuf.format().code))
            .unwrap_or(AttachedBufferKind::RgbOrShm)
    })
}

fn representation_compatible(
    representation: SurfaceRepresentationState,
    buffer: AttachedBufferKind,
) -> bool {
    let explicitly_identity =
        representation
            .coefficients_and_range
            .is_some_and(|(coefficients, range)| {
                coefficients == wp_color_representation_surface_v1::Coefficients::Identity
                    && range == wp_color_representation_surface_v1::Range::Full
            });
    !match buffer {
        AttachedBufferKind::None => false,
        AttachedBufferKind::RgbOrShm => representation.chroma_location.is_some(),
        AttachedBufferKind::Dmabuf(format) if is_rgb_fourcc(format) => {
            representation.chroma_location.is_some()
        }
        // FocalDesk does not yet have a chroma-siting-aware YCbCr import path.
        // Never silently accept metadata that the renderer would ignore.
        AttachedBufferKind::Dmabuf(_) => {
            explicitly_identity || representation.chroma_location.is_some()
        }
    }
}

fn validate_pending_representation(state: &mut DesktopState, surface: &WlSurface) {
    let representation = with_states(surface, |states| {
        *states
            .cached_state
            .get::<SurfaceRepresentationState>()
            .pending()
    });
    if representation_compatible(representation, attached_buffer_kind(surface)) {
        return;
    }

    if let Some(resource) = state
        .color_representation_state
        .surface_objects
        .get(&surface.id())
    {
        resource.post_error(
            wp_color_representation_surface_v1::Error::PixelFormat,
            "surface representation is incompatible with the committed buffer format",
        );
    }
}

fn unset_pending(surface: &WlSurface) {
    if !surface.is_alive() {
        return;
    }
    with_states(surface, |states| {
        *states
            .cached_state
            .get::<SurfaceRepresentationState>()
            .pending() = SurfaceRepresentationState::default();
    });
}

impl GlobalDispatch<wp_color_representation_manager_v1::WpColorRepresentationManagerV1, ()>
    for DesktopState
{
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<wp_color_representation_manager_v1::WpColorRepresentationManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_representation_surface_v1::{AlphaMode, Coefficients, Range};

        let manager = data_init.init(resource, ());
        manager.supported_alpha_mode(AlphaMode::PremultipliedElectrical);
        manager.supported_coefficients_and_ranges(Coefficients::Identity, Range::Full);
        manager.done();
    }
}

impl Dispatch<wp_color_representation_manager_v1::WpColorRepresentationManagerV1, ()>
    for DesktopState
{
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wp_color_representation_manager_v1::WpColorRepresentationManagerV1,
        request: wp_color_representation_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_color_representation_manager_v1::Request::Destroy => {}
            wp_color_representation_manager_v1::Request::GetSurface { id, surface } => {
                if state
                    .color_representation_state
                    .surface_objects
                    .contains_key(&surface.id())
                {
                    resource.post_error(
                        wp_color_representation_manager_v1::Error::SurfaceExists,
                        "a color representation object already exists for this surface",
                    );
                    data_init.init(id, OrphanSurfaceRepresentation);
                    return;
                }

                with_states(&surface, |states| {
                    drop(states.cached_state.get::<SurfaceRepresentationState>());
                });
                add_pre_commit_hook::<DesktopState, _>(&surface, |state, _dh, surface| {
                    validate_pending_representation(state, surface);
                });
                add_destruction_hook::<DesktopState, _>(&surface, |state, surface| {
                    state
                        .color_representation_state
                        .surface_objects
                        .remove(&surface.id());
                });

                let representation = data_init.init(
                    id,
                    SurfaceRepresentation {
                        surface: surface.clone(),
                    },
                );
                state
                    .color_representation_state
                    .surface_objects
                    .insert(surface.id(), representation);
            }
            _ => {}
        }
    }
}

impl
    Dispatch<
        wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
        SurfaceRepresentation,
    > for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
        request: wp_color_representation_surface_v1::Request,
        representation: &SurfaceRepresentation,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if !representation.surface.is_alive() {
            if !matches!(
                request,
                wp_color_representation_surface_v1::Request::Destroy
            ) {
                resource.post_error(
                    wp_color_representation_surface_v1::Error::Inert,
                    "wl_surface is destroyed",
                );
            }
            return;
        }

        match request {
            wp_color_representation_surface_v1::Request::Destroy => {
                unset_pending(&representation.surface);
            }
            wp_color_representation_surface_v1::Request::SetAlphaMode { alpha_mode } => {
                let WEnum::Value(alpha_mode) = alpha_mode else {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::AlphaMode,
                        "unknown alpha mode",
                    );
                    return;
                };
                if alpha_mode
                    != wp_color_representation_surface_v1::AlphaMode::PremultipliedElectrical
                {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::AlphaMode,
                        "unsupported alpha mode",
                    );
                    return;
                }
                with_states(&representation.surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceRepresentationState>()
                        .pending()
                        .alpha_mode = Some(alpha_mode);
                });
            }
            wp_color_representation_surface_v1::Request::SetCoefficientsAndRange {
                coefficients,
                range,
            } => {
                let (WEnum::Value(coefficients), WEnum::Value(range)) = (coefficients, range)
                else {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Coefficients,
                        "unknown coefficients or range",
                    );
                    return;
                };
                if coefficients != wp_color_representation_surface_v1::Coefficients::Identity
                    || range != wp_color_representation_surface_v1::Range::Full
                {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Coefficients,
                        "unsupported coefficients and range",
                    );
                    return;
                }
                with_states(&representation.surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceRepresentationState>()
                        .pending()
                        .coefficients_and_range = Some((coefficients, range));
                });
            }
            wp_color_representation_surface_v1::Request::SetChromaLocation { chroma_location } => {
                let WEnum::Value(chroma_location) = chroma_location else {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::ChromaLocation,
                        "unknown chroma location",
                    );
                    return;
                };
                with_states(&representation.surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceRepresentationState>()
                        .pending()
                        .chroma_location = Some(chroma_location);
                });
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: backend::ClientId,
        _resource: &wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
        representation: &SurfaceRepresentation,
    ) {
        state
            .color_representation_state
            .surface_objects
            .remove(&representation.surface.id());
    }
}

impl
    Dispatch<
        wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
        OrphanSurfaceRepresentation,
    > for DesktopState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
        _request: wp_color_representation_surface_v1::Request,
        _data: &OrphanSurfaceRepresentation,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_rgb_fourcc, representation_compatible, AttachedBufferKind, SurfaceRepresentationState,
    };
    use smithay::backend::allocator::Fourcc;
    use wayland_protocols::wp::color_representation::v1::server::wp_color_representation_surface_v1::{
        ChromaLocation, Coefficients, Range,
    };

    #[test]
    fn advertised_identity_path_accepts_rgb_formats() {
        assert!(is_rgb_fourcc(Fourcc::Argb8888));
        assert!(is_rgb_fourcc(Fourcc::Xbgr2101010));
        assert!(is_rgb_fourcc(Fourcc::Abgr16161616f));
    }

    #[test]
    fn advertised_identity_path_rejects_yuv_formats() {
        assert!(!is_rgb_fourcc(Fourcc::Nv12));
        assert!(!is_rgb_fourcc(Fourcc::P010));
    }

    #[test]
    fn untagged_yuv_remains_compositor_defined() {
        assert!(representation_compatible(
            SurfaceRepresentationState::default(),
            AttachedBufferKind::Dmabuf(Fourcc::Nv12),
        ));
    }

    #[test]
    fn explicit_rgb_identity_rejects_yuv() {
        let representation = SurfaceRepresentationState {
            coefficients_and_range: Some((Coefficients::Identity, Range::Full)),
            ..SurfaceRepresentationState::default()
        };
        assert!(!representation_compatible(
            representation,
            AttachedBufferKind::Dmabuf(Fourcc::Nv12),
        ));
    }

    #[test]
    fn chroma_location_rejects_attached_buffers_but_not_an_empty_surface() {
        let representation = SurfaceRepresentationState {
            chroma_location: Some(ChromaLocation::Type0),
            ..SurfaceRepresentationState::default()
        };
        assert!(!representation_compatible(
            representation,
            AttachedBufferKind::RgbOrShm,
        ));
        assert!(representation_compatible(
            representation,
            AttachedBufferKind::None,
        ));
        assert!(!representation_compatible(
            representation,
            AttachedBufferKind::Dmabuf(Fourcc::Nv12),
        ));
    }
}

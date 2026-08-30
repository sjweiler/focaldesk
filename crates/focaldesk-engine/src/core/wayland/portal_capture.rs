#![allow(unused_imports)]

//! Desktop portal support: `ext-image-copy-capture-v1`, `ext-image-capture-source-v1`, and
//! `wlr-layer-shell-unstable-v1` (used by [`xdg-desktop-portal-wlr`](https://github.com/emersion/xdg-desktop-portal-wlr)).

use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::utils::{Buffer, Size};
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
    OutputCaptureSourceHandler, OutputCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, DmabufConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState,
    Session, SessionRef,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};

use crate::core::desktop::DesktopState;
use crate::core::portal::{self, attach_output_to_capture_source};

impl ImageCaptureSourceHandler for DesktopState {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

impl OutputCaptureSourceHandler for DesktopState {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        attach_output_to_capture_source(&source, output);
    }
}

impl ImageCopyCaptureHandler for DesktopState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        use smithay::output::WeakOutput;
        let weak_output = source.user_data().get::<WeakOutput>()?;
        let output = weak_output.upgrade()?;
        let capture_size = self
            .outputs
            .values()
            .find(|desk_output| desk_output.handle == output)
            .map(|desk_output| {
                Size::<i32, Buffer>::from((
                    desk_output.physical_size.w,
                    desk_output.physical_size.h,
                ))
            })
            .or_else(|| {
                let mode = output.current_mode()?;
                Some(Size::<i32, Buffer>::from((mode.size.w, mode.size.h)))
            })?;

        let dma = self
            .dmabuf_node
            .filter(|_| !self.portal_dmabuf_formats.is_empty())
            .map(|node| DmabufConstraints {
                node,
                formats: self.portal_dmabuf_formats.clone(),
            });

        // On DRM, prefer zero-copy dmabuf capture for OBS / xdg-desktop-portal-wlr. SHM would
        // require a GPU readback (`copy_texture`) on every frame.
        let wide_gamut = portal::portal_capture_color_mode().requires_ten_bit();
        let shm = if wide_gamut
            || (dma.is_some() && self.backend_kind == focaldesk_flow::keybinds::BackendKind::Drm)
        {
            vec![]
        } else {
            vec![
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            ]
        };

        Some(BufferConstraints {
            size: capture_size,
            shm,
            dma,
        })
    }

    fn new_session(&mut self, session: Session) {
        self.image_copy_capture_sessions.push(session);
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.image_copy_capture_sessions
            .retain(|stored| *stored != session);
        focaldesk_logging::flog("portal capture session destroyed");
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let Some(output_id) = portal::output_id_for_session(self, session) else {
            frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
            return;
        };

        portal::try_render_portal_frame(self, frame, output_id);
    }
}

impl WlrLayerShellHandler for DesktopState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        use smithay::desktop::layer_map_for_output;
        use smithay::desktop::LayerSurface;

        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.outputs.values().next().map(|s| s.handle.clone()))
            .or_else(|| self.space.outputs().next().cloned());

        let Some(output) = output else {
            return;
        };

        let mut map = layer_map_for_output(&output);
        // LayerMap assigns intersecting exclusive edges in insertion order.
        // The panel owns the top-left corner, so when it reconnects after an
        // existing dock, temporarily remove the dock and map it again after
        // the panel. This keeps the panel full-width and starts the dock below
        // it regardless of process startup/restart timing.
        let remap_docks = if namespace == crate::core::wayland::trusted_shell::PANEL_NAMESPACE {
            map.layers()
                .filter(|mapped| {
                    mapped.namespace() == crate::core::wayland::trusted_shell::DOCK_NAMESPACE
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for dock in &remap_docks {
            map.unmap_layer(dock);
        }
        let layer = LayerSurface::new(surface, namespace);
        if let Err(error) = map.map_layer(&layer) {
            focaldesk_logging::flog(format!("failed to map layer-shell surface: {error}"));
            return;
        }
        for dock in &remap_docks {
            if let Err(error) = map.map_layer(dock) {
                focaldesk_logging::flog(format!(
                    "failed to restore trusted dock layer ordering: {error}"
                ));
            }
        }

        // Do not configure here. This callback runs when get_layer_surface is
        // handled, before the client's set_size/set_anchor requests and first
        // wl_surface.commit. The compositor commit handler arranges the layer
        // using that committed state and sends the initial configure there.
    }

    fn new_popup(
        &mut self,
        _parent: WlrLayerSurface,
        popup: smithay::wayland::shell::xdg::PopupSurface,
    ) {
        use smithay::desktop::{PopupKind, PopupManager};

        // xdg-shell announces the popup before wlr-layer-shell assigns its
        // layer parent, so the first tracking attempt cannot find a root.
        // Register it now that get_popup has established that relationship.
        self.unconstrain_popup(&popup);
        if let Err(error) = self.popups.track_popup(PopupKind::from(popup)) {
            focaldesk_logging::flog(format!("failed to track layer-shell popup: {error}"));
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        use smithay::desktop::layer_map_for_output;

        if let Some((mut map, layer)) = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            layer.map(|l| (map, l))
        }) {
            map.unmap_layer(&layer);
        }
    }
}

//! Desktop portal support: `ext-image-copy-capture-v1`, `ext-image-capture-source-v1`, and
//! `wlr-layer-shell-unstable-v1` (used by [`xdg-desktop-portal-wlr`](https://github.com/emersion/xdg-desktop-portal-wlr)).

use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::utils::Transform;
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
    OutputCaptureSourceHandler, OutputCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};

use crate::core::desktop::DesktopState;
use crate::core::portal::{self, attach_output_to_capture_source};

impl ImageCaptureSourceHandler for DesktopState {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}
smithay::delegate_image_capture_source!(DesktopState);

impl OutputCaptureSourceHandler for DesktopState {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        attach_output_to_capture_source(&source, output);
    }
}
smithay::delegate_output_capture_source!(DesktopState);

impl ImageCopyCaptureHandler for DesktopState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        use smithay::output::WeakOutput;
        let weak_output = source.user_data().get::<WeakOutput>()?;
        let output = weak_output.upgrade()?;
        let mode = output.current_mode()?;

        Some(BufferConstraints {
            size: mode.size.to_logical(1).to_buffer(1, Transform::Normal),
            shm: vec![
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            ],
            dma: None,
        })
    }

    fn new_session(&mut self, session: Session) {}

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let Some(output_id) = portal::output_id_for_session(self, session) else {
            return;
        };

        portal::try_render_portal_frame(self, frame, output_id);
    }
}
smithay::delegate_image_copy_capture!(DesktopState);

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
        let _ = map.map_layer(&LayerSurface::new(surface, namespace));
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
smithay::delegate_layer_shell!(DesktopState);

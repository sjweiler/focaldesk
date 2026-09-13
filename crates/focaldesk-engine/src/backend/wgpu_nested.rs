//! Nested Vulkan backend built on FocalDesk's renderer boundary and wgpu.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_render::{
    FramePixelFormat, PresentRenderer, PresentResult, TextureQuad, WgpuVulkanRenderer,
};
use focaldesk_types::OutputId;
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::desktop::{layer_map_for_output, PopupManager};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Size, Transform};
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use smithay::wayland::shm::with_buffer_contents;
use tracing::{error, info, trace, warn};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    platform::scancode::PhysicalKeyExtScancode,
    window::{Window, WindowId},
};

use super::common::{
    bootstrap_compositor_core, client_state_from_stream, is_nonfatal_wayland_io_error,
    BootstrapOutput, NestedDesktop,
};
use crate::core::input::{
    FlowInputEvent, FlowKeyState, FlowModifiers, FlowMouseButton, FlowScrollDelta,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
struct NestedVulkanApp {
    window: Option<Arc<Window>>,
    renderer: Option<WgpuVulkanRenderer>,
    desktop: Option<NestedDesktop>,
    logged_first_present: bool,
    logged_first_surface_present: bool,
    logged_first_cursor_present: bool,
    modifiers: FlowModifiers,
    fatal_error: Option<anyhow::Error>,
}

impl NestedVulkanApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        error!(%error, "nested wgpu Vulkan backend failed");
        self.fatal_error = Some(error);
        event_loop.exit();
    }

    fn dispatch_wayland(&mut self) -> Result<bool> {
        let Some(desktop) = self.desktop.as_mut() else {
            return Ok(false);
        };

        while let Some(stream) = desktop.listener.accept()? {
            let client_state = client_state_from_stream(&stream);
            let client = desktop
                .display
                .handle()
                .insert_client(stream, Arc::new(client_state))?;
            desktop.clients.push(client);
            trace!(
                target: "focaldesk",
                clients = desktop.clients.len(),
                "accepted nested wgpu Wayland client"
            );
        }

        if desktop.state.wayland_clients_may_dispatch() {
            if let Err(error) = desktop.display.dispatch_clients(&mut desktop.state) {
                if !is_nonfatal_wayland_io_error(&error) {
                    return Err(error.into());
                }
                warn!(%error, "ignoring nonfatal Wayland dispatch error");
            }
            crate::core::wayland::color_management_protocol::flush_pending_image_description_info_done(
                &mut desktop.state,
            );
        }

        desktop.state.process_deferred_window_ops();
        desktop.state.refresh_space();
        desktop.state.tick_layout();
        if let Err(error) = desktop.display.flush_clients() {
            if !is_nonfatal_wayland_io_error(&error) {
                return Err(error.into());
            }
            warn!(%error, "ignoring nonfatal Wayland flush error");
        }

        Ok(desktop.state.needs_redraw() || !desktop.clients.is_empty())
    }

    fn resize_output(&mut self, width: u32, height: u32, scale_factor: f64) {
        if width == 0 || height == 0 {
            return;
        }
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        let Some(output) = desktop
            .state
            .outputs
            .get(&desktop.state.primary_output)
            .map(|output| output.handle.clone())
        else {
            return;
        };
        desktop.state.set_output_from_nested(
            output,
            Size::<i32, Physical>::from((width as i32, height as i32)),
            scale_factor,
        );
        desktop.state.handle_input(FlowInputEvent::Resized {
            output_id: OutputId(1),
            width,
            height,
            scale_factor,
        });
    }

    fn handle_input(&mut self, event: FlowInputEvent) {
        if let Some(desktop) = self.desktop.as_mut() {
            desktop.state.handle_input(event);
        }
    }
}

impl ApplicationHandler for NestedVulkanApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("FocalDesk — nested wgpu/Vulkan")
            .with_inner_size(LogicalSize::new(1280, 800));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(
                    event_loop,
                    anyhow!(error).context("create nested Vulkan window"),
                );
                return;
            }
        };
        let renderer = match WgpuVulkanRenderer::new(window.clone()) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.fail(event_loop, error);
                return;
            }
        };
        let window_size = window.inner_size();
        let desktop = match bootstrap_compositor_core(
            Some(BootstrapOutput {
                name: "focaldesk-wgpu".into(),
                buffer_size: Size::<i32, Physical>::from((
                    window_size.width as i32,
                    window_size.height as i32,
                )),
                scale_factor: window.scale_factor(),
            }),
            BackendKind::Winit,
        ) {
            Ok(desktop) => desktop,
            Err(error) => {
                self.fail(
                    event_loop,
                    error.context("bootstrap Wayland compositor core"),
                );
                return;
            }
        };

        let info = renderer.info();
        info!(
            target: "focaldesk",
            adapter = %info.adapter_name,
            driver = %info.driver,
            driver_info = %info.driver_info,
            surface_format = %info.surface_format,
            wayland_display = %desktop.wayland_display,
            "initialized nested wgpu Vulkan compositor"
        );
        window.set_cursor_visible(false);
        window.request_redraw();
        self.renderer = Some(renderer);
        self.desktop = Some(desktop);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                self.resize_output(size.width, size.height, window.scale_factor());
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = window.inner_size();
                self.resize_output(size.width, size.height, scale_factor);
                window.request_redraw();
            }
            WindowEvent::CursorEntered { .. } => {
                self.handle_input(FlowInputEvent::PointerEntered);
                window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.handle_input(FlowInputEvent::PointerLeft);
                window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = window.scale_factor();
                let logical_position = position.to_logical::<f64>(scale);
                self.handle_input(FlowInputEvent::PointerMoved {
                    position: Point::from((logical_position.x, logical_position.y)),
                    delta: None,
                    delta_unaccel: None,
                });
                window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => FlowMouseButton::Left,
                    MouseButton::Right => FlowMouseButton::Right,
                    MouseButton::Middle => FlowMouseButton::Middle,
                    MouseButton::Back => FlowMouseButton::Back,
                    MouseButton::Forward => FlowMouseButton::Forward,
                    MouseButton::Other(button) => FlowMouseButton::Other(button),
                };
                let state = match state {
                    ElementState::Pressed => FlowKeyState::Pressed,
                    ElementState::Released => FlowKeyState::Released,
                };
                let position = self
                    .desktop
                    .as_ref()
                    .map(|desktop| desktop.state.input.pointer_pos)
                    .unwrap_or_default();
                self.handle_input(FlowInputEvent::PointerButton {
                    button,
                    state,
                    position,
                });
                window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => FlowScrollDelta::Line { x, y: -y },
                    MouseScrollDelta::PixelDelta(delta) => {
                        let logical = delta.to_logical::<f64>(window.scale_factor());
                        FlowScrollDelta::Pixel {
                            x: logical.x,
                            y: -logical.y,
                        }
                    }
                };
                let position = self
                    .desktop
                    .as_ref()
                    .map(|desktop| desktop.state.input.pointer_pos)
                    .unwrap_or_default();
                self.handle_input(FlowInputEvent::PointerScroll { delta, position });
                window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.modifiers = FlowModifiers {
                    shift: state.shift_key(),
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                    super_key: state.super_key(),
                };
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Clients synthesize repeats from wl_keyboard repeat_info.
                // Treating host repeats as fresh presses would repeat twice.
                if !event.repeat {
                    let Some(keycode) = event.physical_key.to_scancode() else {
                        return;
                    };
                    self.handle_input(FlowInputEvent::Key {
                        keycode,
                        state: match event.state {
                            ElementState::Pressed => FlowKeyState::Pressed,
                            ElementState::Released => FlowKeyState::Released,
                        },
                        repeat: false,
                        modifiers: self.modifiers,
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::Focused(true) => {
                if let Some(desktop) = self.desktop.as_mut() {
                    desktop.state.handle_session_resume();
                }
            }
            WindowEvent::RedrawRequested => {
                let mut surfaces = self
                    .desktop
                    .as_ref()
                    .map(collect_shm_surfaces)
                    .unwrap_or_default();
                let client_surface_count = surfaces.len();
                let cursor_present =
                    if let Some(cursor) = self.desktop.as_mut().and_then(cursor_texture_quad) {
                        surfaces.push(cursor);
                        true
                    } else {
                        false
                    };
                let result = self
                    .renderer
                    .as_mut()
                    .context("renderer missing after nested window initialization")
                    .and_then(|renderer| renderer.present_frame(&surfaces));
                match result {
                    Ok(PresentResult::Presented) => {
                        if !self.logged_first_present {
                            info!(target: "focaldesk", "presented first nested wgpu Vulkan frame");
                            self.logged_first_present = true;
                        }
                        if client_surface_count > 0 && !self.logged_first_surface_present {
                            info!(
                                target: "focaldesk",
                                surfaces = client_surface_count,
                                "composited first Wayland SHM surface with wgpu"
                            );
                            self.logged_first_surface_present = true;
                        }
                        if cursor_present && !self.logged_first_cursor_present {
                            info!(target: "focaldesk", "composited first wgpu software cursor");
                            self.logged_first_cursor_present = true;
                        }
                        if let Some(desktop) = self.desktop.as_mut() {
                            desktop.state.clear_repaint_request();
                            desktop.state.render.frame_no += 1;
                            let frame_time_ms = desktop.start.elapsed().as_millis() as u32;
                            desktop.state.send_frame_callbacks(frame_time_ms);
                        }
                    }
                    Ok(PresentResult::Skipped) => {}
                    Ok(PresentResult::SurfaceReconfigured) => window.request_redraw(),
                    Err(error) => self.fail(event_loop, error),
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.dispatch_wayland() {
            Ok(true) => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            Ok(false) => {}
            Err(error) => {
                self.fail(event_loop, error);
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME_INTERVAL));
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        warn!("nested wgpu Vulkan window suspended");
        self.desktop = None;
        self.renderer = None;
        self.window = None;
        self.logged_first_present = false;
        self.logged_first_surface_present = false;
        self.logged_first_cursor_present = false;
    }
}

fn cursor_texture_quad(desktop: &mut NestedDesktop) -> Option<TextureQuad> {
    if !desktop.state.cursor_manager.visible() {
        return None;
    }
    let (pointer_x, pointer_y) = desktop.state.cursor_manager.position();
    let output = desktop
        .state
        .outputs
        .get(&desktop.state.focused_output)
        .or_else(|| desktop.state.outputs.get(&desktop.state.primary_output))?;
    let scale = output.scale_factor;
    let output_origin = output.logical_origin;
    let image = desktop.state.cursor_manager.current_image().ok()?;
    let x =
        ((pointer_x - f64::from(output_origin.x)) * scale).round() as i32 - image.hotspot_x as i32;
    let y =
        ((pointer_y - f64::from(output_origin.y)) * scale).round() as i32 - image.hotspot_y as i32;

    Some(TextureQuad {
        pixels: image.rgba.clone(),
        width: image.width,
        height: image.height,
        stride: image.width.saturating_mul(4),
        format: FramePixelFormat::Rgba8Srgb,
        destination: [x, y, image.width as i32, image.height as i32],
        source_uv: [0.0, 0.0, 1.0, 1.0],
    })
}

fn collect_shm_surfaces(desktop: &NestedDesktop) -> Vec<TextureQuad> {
    let output_state = desktop.state.outputs.get(&desktop.state.primary_output);
    let scale = output_state
        .map(|output| output.scale_factor)
        .unwrap_or(1.0);

    let mut output = Vec::new();
    if let Some(output_state) = output_state {
        collect_shm_layers(
            &output_state.handle,
            &[WlrLayer::Background, WlrLayer::Bottom],
            scale,
            &mut output,
        );
    }
    for window in desktop.state.space.elements() {
        let Some(window_location) = desktop.state.space.element_location(window) else {
            continue;
        };
        let Some(root) = window.wl_surface() else {
            continue;
        };
        let window_origin = window_location - window.geometry().loc;

        // Smithay's downward traversal and popup iterator are front-to-back,
        // while wgpu draws in painter's order. Build one front-to-back window
        // list, then reverse it before appending to the back-to-front Space.
        let mut window_surfaces = Vec::new();
        for (popup, popup_offset) in PopupManager::popups_for_surface(&root) {
            let popup_origin = window_origin + popup_offset - popup.geometry().loc;
            collect_shm_surface_tree(
                popup.wl_surface(),
                popup_origin,
                scale,
                &mut window_surfaces,
            );
        }
        collect_shm_surface_tree(&root, window_origin, scale, &mut window_surfaces);
        output.extend(window_surfaces.into_iter().rev());
    }
    if let Some(output_state) = output_state {
        collect_shm_layers(
            &output_state.handle,
            &[WlrLayer::Top, WlrLayer::Overlay],
            scale,
            &mut output,
        );
    }
    output
}

fn collect_shm_layers(
    output_handle: &smithay::output::Output,
    layer_kinds: &[WlrLayer],
    scale: f64,
    output: &mut Vec<TextureQuad>,
) {
    let layer_map = layer_map_for_output(output_handle);
    for &layer_kind in layer_kinds {
        for layer in layer_map.layers_on(layer_kind) {
            let Some(geometry) = layer_map.layer_geometry(layer) else {
                continue;
            };
            let root = layer.wl_surface();
            let mut layer_surfaces = Vec::new();
            for (popup, popup_offset) in PopupManager::popups_for_surface(root) {
                let popup_origin = geometry.loc + popup_offset - popup.geometry().loc;
                collect_shm_surface_tree(
                    popup.wl_surface(),
                    popup_origin,
                    scale,
                    &mut layer_surfaces,
                );
            }
            collect_shm_surface_tree(root, geometry.loc, scale, &mut layer_surfaces);
            output.extend(layer_surfaces.into_iter().rev());
        }
    }
}

fn collect_shm_surface_tree(
    root: &WlSurface,
    origin: Point<i32, Logical>,
    scale: f64,
    output: &mut Vec<TextureQuad>,
) {
    with_surface_tree_downward(
        root,
        origin,
        |_, states, location| {
            let view = states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .and_then(|state| state.lock().ok()?.view());
            match view {
                Some(view) => TraversalAction::DoChildren(*location + view.offset),
                None => TraversalAction::SkipChildren,
            }
        },
        |_, states, location| {
            let Some(renderer_state) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let Ok(renderer_state) = renderer_state.lock() else {
                return;
            };
            let (Some(buffer), Some(view), Some(buffer_size)) = (
                renderer_state.buffer().cloned(),
                renderer_state.view(),
                renderer_state.buffer_size(),
            ) else {
                return;
            };
            // Buffer rotation/reflection needs corresponding UV transforms in
            // the renderer. Until those land, skip instead of displaying the
            // client's pixels with the wrong orientation.
            if renderer_state.buffer_transform() != Transform::Normal {
                return;
            }
            if buffer_size.w <= 0 || buffer_size.h <= 0 {
                return;
            }
            let surface_location = *location + view.offset;
            drop(renderer_state);

            let destination = [
                (f64::from(surface_location.x) * scale).round() as i32,
                (f64::from(surface_location.y) * scale).round() as i32,
                (f64::from(view.dst.w) * scale).round() as i32,
                (f64::from(view.dst.h) * scale).round() as i32,
            ];
            let source_uv = [
                (view.src.loc.x / f64::from(buffer_size.w)) as f32,
                (view.src.loc.y / f64::from(buffer_size.h)) as f32,
                ((view.src.loc.x + view.src.size.w) / f64::from(buffer_size.w)) as f32,
                ((view.src.loc.y + view.src.size.h) / f64::from(buffer_size.h)) as f32,
            ];
            if let Some(frame) = shm_surface_frame(&buffer, destination, source_uv) {
                output.push(frame);
            }
        },
        |_, _, _| true,
    );
}

fn shm_surface_frame(
    buffer: &smithay::backend::renderer::utils::Buffer,
    destination: [i32; 4],
    source_uv: [f32; 4],
) -> Option<TextureQuad> {
    with_buffer_contents(buffer, |ptr, len, data| {
        if !matches!(
            data.format,
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
        ) || data.offset < 0
            || data.width <= 0
            || data.height <= 0
            || data.stride < data.width.saturating_mul(4)
        {
            return None;
        }

        let offset = usize::try_from(data.offset).ok()?;
        let stride = usize::try_from(data.stride).ok()?;
        let height = usize::try_from(data.height).ok()?;
        let byte_len = stride.checked_mul(height)?;
        let end = offset.checked_add(byte_len)?;
        if end > len {
            return None;
        }

        // SAFETY: Smithay keeps the SHM mapping valid for this callback, and the
        // checked range is contained in the supplied pool length. Copying here
        // ensures no client-owned pointer escapes the callback.
        let source = unsafe { std::slice::from_raw_parts(ptr.add(offset), byte_len) };
        let mut pixels = source.to_vec();

        #[cfg(target_endian = "little")]
        if data.format == wl_shm::Format::Xrgb8888 {
            for row in pixels.chunks_exact_mut(stride) {
                for pixel in row[..data.width as usize * 4].chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
            }
        }

        #[cfg(target_endian = "big")]
        for row in pixels.chunks_exact_mut(stride) {
            for pixel in row[..data.width as usize * 4].chunks_exact_mut(4) {
                let [a, r, g, b] = [pixel[0], pixel[1], pixel[2], pixel[3]];
                pixel.copy_from_slice(&[
                    b,
                    g,
                    r,
                    if data.format == wl_shm::Format::Xrgb8888 {
                        255
                    } else {
                        a
                    },
                ]);
            }
        }

        Some(TextureQuad {
            pixels,
            width: data.width as u32,
            height: data.height as u32,
            stride: data.stride as u32,
            format: FramePixelFormat::Bgra8Srgb,
            destination,
            source_uv,
        })
    })
    .ok()
    .flatten()
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new().context("create nested Vulkan event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = NestedVulkanApp::default();
    event_loop
        .run_app(&mut app)
        .context("run nested Vulkan event loop")?;
    if let Some(error) = app.fatal_error {
        return Err(error.into());
    }
    Ok(())
}

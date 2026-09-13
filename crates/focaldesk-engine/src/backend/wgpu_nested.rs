//! Nested Vulkan backend built on FocalDesk's renderer boundary and wgpu.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_render::{PresentRenderer, PresentResult, ShmSurfaceFrame, WgpuVulkanRenderer};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Physical, Size};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shm::with_buffer_contents;
use tracing::{error, info, trace, warn};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

use super::common::{
    bootstrap_compositor_core, client_state_from_stream, is_nonfatal_wayland_io_error,
    BootstrapOutput, NestedDesktop,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
struct NestedVulkanApp {
    window: Option<Arc<Window>>,
    renderer: Option<WgpuVulkanRenderer>,
    desktop: Option<NestedDesktop>,
    logged_first_present: bool,
    logged_first_surface_present: bool,
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
            WindowEvent::RedrawRequested => {
                let surfaces = self
                    .desktop
                    .as_ref()
                    .map(collect_shm_surfaces)
                    .unwrap_or_default();
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
                        if !surfaces.is_empty() && !self.logged_first_surface_present {
                            info!(
                                target: "focaldesk",
                                surfaces = surfaces.len(),
                                "composited first Wayland SHM surface with wgpu"
                            );
                            self.logged_first_surface_present = true;
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
    }
}

fn collect_shm_surfaces(desktop: &NestedDesktop) -> Vec<ShmSurfaceFrame> {
    let scale = desktop
        .state
        .outputs
        .get(&desktop.state.primary_output)
        .map(|output| output.scale_factor)
        .unwrap_or(1.0);

    desktop
        .state
        .space
        .elements()
        .filter_map(|window| {
            let location = desktop.state.space.element_location(window)?;
            let surface = window.wl_surface()?;
            let (buffer, logical_size) = with_renderer_surface_state(&surface, |state| {
                (state.buffer().cloned(), state.surface_size())
            })?;
            let buffer = buffer?;
            let logical_size = logical_size?;
            let destination = [
                (f64::from(location.x) * scale).round() as i32,
                (f64::from(location.y) * scale).round() as i32,
                (f64::from(logical_size.w) * scale).round() as i32,
                (f64::from(logical_size.h) * scale).round() as i32,
            ];
            shm_surface_frame(&buffer, destination)
        })
        .collect()
}

fn shm_surface_frame(
    buffer: &smithay::backend::renderer::utils::Buffer,
    destination: [i32; 4],
) -> Option<ShmSurfaceFrame> {
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

        Some(ShmSurfaceFrame {
            pixels,
            width: data.width as u32,
            height: data.height as u32,
            stride: data.stride as u32,
            destination,
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

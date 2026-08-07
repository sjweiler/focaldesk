#![allow(dead_code, deprecated)]

//! Standalone GLES shell renderer for `focal-panel` and `focal-dock`.
//!
//! Everything under this crate is client-owned: Wayland/EGL setup, render
//! loop, shader sources, icon atlas, IBM Plex font atlas, and shell layout.
//! There is intentionally no dependency on `focaldesk-ui` or the compositor.

mod atlas;
mod chrome;
mod chrome_draw;
mod chrome_layout;
mod chrome_shaders;
mod chrome_theme;
mod controls;
mod font_atlas;
mod fonts;
mod svg;

use anyhow::{anyhow, Context, Result};
use chrome::{dock_slot_rects, Chrome, ChromeMetrics, PulseFrame};
use chrono::Local;
use controls::{clock_control, dock_controls, launcher_control, panel_controls};
use focaldesk_ipc::{send_desktop_request, DesktopAction, IpcRequest, IpcResponse};
use smithay::{
    backend::{
        egl::{
            context::{GlAttributes, PixelFormatRequirements},
            ffi,
            native::{EGLNativeDisplay, EGLPlatform},
            EGLContext, EGLDisplay, EGLSurface,
        },
        renderer::{gles::GlesRenderer, Bind, Color32F, Frame, Renderer},
    },
    utils::{Physical, Rectangle, Size, Transform},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_region, wl_seat, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_egl::WlEglSurface;
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRole {
    Panel,
    Dock,
}

impl ShellRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Panel => "focal-panel",
            Self::Dock => "focal-dock",
        }
    }

    const fn anchor(self) -> Anchor {
        match self {
            Self::Panel => Anchor::TOP.union(Anchor::LEFT).union(Anchor::RIGHT),
            Self::Dock => Anchor::TOP.union(Anchor::LEFT).union(Anchor::BOTTOM),
        }
    }

    const fn preferred_size(self) -> (u32, u32) {
        match self {
            // The transparent extension gives GLES tooltips room outside the
            // visible chrome without reserving additional desktop space.
            Self::Panel => (0, 112),
            Self::Dock => (320, 0),
        }
    }

    const fn exclusive_zone(self) -> i32 {
        match self {
            Self::Panel => 64,
            Self::Dock => 76,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ClickPulse {
    control: usize,
    click: (f64, f64),
    started: Instant,
}

/// Prefer the independent GLES client and enter GTK only after a GLES failure.
pub fn run(role: ShellRole) -> Result<()> {
    if std::env::var_os("FOCALDESK_SHELL_FORCE_GTK").is_some() {
        return run_gtk(role);
    }
    match run_gles(role) {
        Ok(()) => Ok(()),
        Err(error) => {
            eprintln!(
                "{}: independent GLES renderer failed ({error:#}); starting GTK fallback",
                role.namespace()
            );
            run_gtk(role).with_context(|| format!("GTK fallback after GLES failure: {error:#}"))
        }
    }
}

fn run_gtk(role: ShellRole) -> Result<()> {
    focaldesk_shell_gtk::run(match role {
        ShellRole::Panel => focaldesk_shell_gtk::ShellRole::Panel,
        ShellRole::Dock => focaldesk_shell_gtk::ShellRole::Dock,
    })
}

fn run_gles(role: ShellRole) -> Result<()> {
    if !wayland_egl::is_available() {
        return Err(anyhow!("libwayland-egl is unavailable"));
    }
    let conn = Connection::connect_to_env().context("connect to Wayland compositor")?;
    let display_ptr = conn.backend().display_id().as_ptr() as *mut c_void;
    if display_ptr.is_null() {
        return Err(anyhow!("Wayland display has no native EGL pointer"));
    }
    let display = unsafe { EGLDisplay::new(WaylandDisplay(display_ptr)) }
        .context("create independent Wayland EGL display")?;
    let context = EGLContext::new_with_config(
        &display,
        GlAttributes {
            version: (3, 0),
            profile: None,
            debug: cfg!(debug_assertions),
            vsync: true,
        },
        PixelFormatRequirements::_8_bit(),
    )
    .context("create window-configured GLES context")?;
    let renderer = unsafe { GlesRenderer::new(context) }.context("create GLES renderer")?;

    let (globals, mut queue) = registry_queue_init(&conn).context("read Wayland globals")?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor unavailable")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("wlr-layer-shell unavailable")?;
    let output_state = OutputState::new(&globals, &qh);
    let fractional_scale_manager = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();
    let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
    let outputs: Vec<_> = output_state.outputs().collect();
    let targets = if outputs.is_empty() {
        vec![None]
    } else {
        outputs.into_iter().map(Some).collect()
    };

    let config = focaldesk_config::load_config();
    let theme = focaldesk_themes::theme_by_name(&config.appearance.theme);
    let mut chrome = Chrome::new(ChromeMetrics::default());
    chrome.theme = chrome_theme::chrome_theme_from_flow_theme(&theme.chrome);
    eprintln!(
        "{}: theme {} (font scale {:.2})",
        role.namespace(),
        config.appearance.theme,
        config.appearance.font_scale
    );
    let mut client = ShellClient {
        registry_state: RegistryState::new(&globals),
        compositor,
        layer_shell,
        output_state,
        seat_state: SeatState::new(&globals, &qh),
        fractional_scale_manager,
        viewporter,
        // Field declaration order below controls safe EGL/Wayland destruction.
        egl_surfaces: Vec::new(),
        chrome,
        renderer,
        layers: Vec::new(),
        layer_outputs: Vec::new(),
        sizes: Vec::new(),
        scales: Vec::new(),
        fractional_scales: Vec::new(),
        viewports: Vec::new(),
        configured: Vec::new(),
        hovered: Vec::new(),
        pulses: Vec::new(),
        role,
        closed: false,
        fatal_error: None,
        pointer: None,
        workspace_count: 1,
        active_workspace: 1,
        shell: focaldesk_ipc::ShellSnapshot::default(),
        last_snapshot: Instant::now() - Duration::from_secs(10),
        font_scale: config.appearance.font_scale.max(0.5),
        ready_reported: false,
    };
    for output in targets {
        client.add_surface(&qh, output);
    }
    while !client.closed && client.fatal_error.is_none() {
        queue
            .blocking_dispatch(&mut client)
            .context("dispatch shell events")?;
    }
    client
        .fatal_error
        .take()
        .map_or(Ok(()), |e| Err(anyhow!(e)))
}

#[derive(Debug)]
struct WaylandDisplay(*mut c_void);

unsafe impl Send for WaylandDisplay {}

impl EGLNativeDisplay for WaylandDisplay {
    fn supported_platforms(&self) -> Vec<EGLPlatform<'_>> {
        vec![
            EGLPlatform::new(
                ffi::egl::PLATFORM_WAYLAND_KHR,
                "PLATFORM_WAYLAND_KHR",
                self.0,
                vec![ffi::egl::NONE as ffi::EGLint],
                &["EGL_KHR_platform_wayland"],
            ),
            EGLPlatform::new(
                ffi::egl::PLATFORM_WAYLAND_EXT,
                "PLATFORM_WAYLAND_EXT",
                self.0,
                vec![ffi::egl::NONE as ffi::EGLint],
                &["EGL_EXT_platform_wayland"],
            ),
        ]
    }
}

struct ShellClient {
    registry_state: RegistryState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    seat_state: SeatState,
    fractional_scale_manager: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    // EGL windows and GL objects drop before the context; the context drops
    // before the wl_surfaces that back those windows.
    egl_surfaces: Vec<Option<EGLSurface>>,
    chrome: Chrome,
    renderer: GlesRenderer,
    layers: Vec<LayerSurface>,
    layer_outputs: Vec<Option<wl_output::WlOutput>>,
    sizes: Vec<(u32, u32)>,
    scales: Vec<f64>,
    fractional_scales: Vec<Option<WpFractionalScaleV1>>,
    viewports: Vec<Option<WpViewport>>,
    configured: Vec<bool>,
    hovered: Vec<Option<usize>>,
    pulses: Vec<Option<ClickPulse>>,
    role: ShellRole,
    closed: bool,
    fatal_error: Option<String>,
    pointer: Option<wl_pointer::WlPointer>,
    workspace_count: usize,
    active_workspace: u32,
    shell: focaldesk_ipc::ShellSnapshot,
    last_snapshot: Instant,
    font_scale: f64,
    ready_reported: bool,
}

impl ShellClient {
    fn fail(&mut self, context: &str, error: impl std::fmt::Display) {
        if self.fatal_error.is_none() {
            self.fatal_error = Some(format!("{context}: {error}"));
        }
    }

    fn add_surface(&mut self, qh: &QueueHandle<Self>, output: Option<wl_output::WlOutput>) {
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Top,
            Some(self.role.namespace()),
            output.as_ref(),
        );
        let (width, height) = self.role.preferred_size();
        layer.set_anchor(self.role.anchor());
        layer.set_exclusive_zone(self.role.exclusive_zone());
        layer.set_size(width, height);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        let input_region = self.compositor.wl_compositor().create_region(qh, ());
        match self.role {
            ShellRole::Panel => input_region.add(0, 0, i32::MAX, 64),
            ShellRole::Dock => input_region.add(0, 0, 76, i32::MAX),
        }
        layer.wl_surface().set_input_region(Some(&input_region));
        input_region.destroy();
        layer.commit();
        let fractional_scale = self.fractional_scale_manager.as_ref().map(|manager| {
            manager.get_fractional_scale(layer.wl_surface(), qh, layer.wl_surface().clone())
        });
        let viewport = self
            .viewporter
            .as_ref()
            .map(|viewporter| viewporter.get_viewport(layer.wl_surface(), qh, ()));
        if fractional_scale.is_some() && viewport.is_some() {
            layer.wl_surface().set_buffer_scale(1);
        }
        self.layers.push(layer);
        self.egl_surfaces.push(None);
        self.layer_outputs.push(output);
        self.sizes.push((width, height));
        self.scales.push(1.0);
        self.fractional_scales.push(fractional_scale);
        self.viewports.push(viewport);
        self.configured.push(false);
        self.hovered.push(None);
        self.pulses.push(None);
    }

    fn refresh_snapshot(&mut self) {
        if self.last_snapshot.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_snapshot = Instant::now();
        if let Ok(IpcResponse::DesktopSnapshot { snapshot }) =
            send_desktop_request(&IpcRequest::GetDesktopSnapshot)
        {
            self.workspace_count = snapshot.shell.workspace_count.max(1);
            self.active_workspace = snapshot.session.active_workspace_id.max(1);
            self.shell = snapshot.shell;
        }
    }

    fn ensure_surface(&mut self, index: usize, size: Size<i32, Physical>) -> Result<()> {
        if let Some(surface) = self.egl_surfaces[index].as_ref() {
            if surface.get_size() != Some(size) && !surface.resize(size.w, size.h, 0, 0) {
                return Err(anyhow!("EGL window resize rejected"));
            }
            return Ok(());
        }
        let native = WlEglSurface::new(self.layers[index].wl_surface().id(), size.w, size.h)
            .context("create wl_egl_window")?;
        let context = self.renderer.egl_context();
        let format = context
            .pixel_format()
            .context("configured EGL context has no pixel format")?;
        self.egl_surfaces[index] = Some(
            unsafe { EGLSurface::new(context.display(), format, context.config_id(), native) }
                .context("create EGL window surface")?,
        );
        Ok(())
    }

    fn draw(&mut self, index: usize, qh: &QueueHandle<Self>) -> Result<()> {
        self.refresh_snapshot();
        let scale = self.scales[index].max(1.0);
        let (width, height) = self.sizes[index];
        let size = Size::<i32, Physical>::from((
            (width.max(1) as f64 * scale).round().max(1.0) as i32,
            (height.max(1) as f64 * scale).round().max(1.0) as i32,
        ));
        self.ensure_surface(index, size)?;
        self.chrome
            .ensure_gpu_resources(&mut self.renderer, scale)?;
        self.chrome.ensure_font_resources(&mut self.renderer)?;
        self.chrome.ensure_shader_resources(&mut self.renderer)?;

        let surface = self.egl_surfaces[index].as_mut().unwrap();
        let mut target = self.renderer.bind(surface)?;
        let mut frame = self
            .renderer
            .render(&mut target, size, Transform::Flipped180)?;
        frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)])?;
        let pulse = self.pulses[index].and_then(|pulse| {
            let elapsed = pulse.started.elapsed();
            (elapsed < Duration::from_secs(2)).then_some(PulseFrame {
                control: pulse.control,
                click: pulse.click,
                elapsed,
            })
        });
        if pulse.is_none() {
            self.pulses[index] = None;
        }
        match self.role {
            ShellRole::Panel => {
                let controls = panel_controls(&self.shell);
                self.chrome.render_panel(
                    &mut frame,
                    size,
                    scale,
                    &controls,
                    self.hovered[index],
                    pulse,
                )?;
                let title = self
                    .shell
                    .focused_window_title
                    .as_deref()
                    .unwrap_or("FOCALDESK");
                self.chrome.render_text(
                    &mut frame,
                    &title.chars().take(44).collect::<String>(),
                    (230.0 * scale).round() as i32,
                    (39.0 * scale).round() as i32,
                    size,
                    scale * self.font_scale,
                    [0.92, 0.95, 1.0, 1.0],
                )?;
                self.chrome.render_text(
                    &mut frame,
                    &Local::now().format("%-I:%M %p").to_string(),
                    size.w - (126.0 * scale).round() as i32,
                    (39.0 * scale).round() as i32,
                    size,
                    scale * self.font_scale,
                    [0.92, 0.95, 1.0, 1.0],
                )?;
            }
            ShellRole::Dock => {
                let logical_h = (size.h as f64 / scale).round() as i32;
                let capacity = ((logical_h - 26).max(0) / 56) as usize;
                let controls = dock_controls(self.workspace_count, self.active_workspace, capacity);
                self.chrome.render_dock(
                    &mut frame,
                    size,
                    &controls,
                    self.hovered[index],
                    pulse,
                    scale,
                )?;
            }
        }
        let _ = frame.finish()?;
        drop(target);
        let layer = &self.layers[index];
        layer.wl_surface().frame(qh, layer.wl_surface().clone());
        surface.swap_buffers(None)?;
        Ok(())
    }

    fn control_at(&self, index: usize, x: f64, y: f64) -> Option<usize> {
        match self.role {
            ShellRole::Dock => {
                if !(0.0..76.0).contains(&x) {
                    return None;
                }
                let logical_h = self.sizes[index].1 as i32;
                let capacity = ((logical_h - 26).max(0) / 56) as usize;
                let controls = dock_controls(self.workspace_count, self.active_workspace, capacity);
                let point = (x as i32, y as i32);
                dock_slot_rects(76, logical_h, controls.len())
                    .iter()
                    .position(|slot| slot.contains(point))
            }
            ShellRole::Panel => {
                if !(0.0..64.0).contains(&y) {
                    return None;
                }
                let logical = Size::from((self.sizes[index].0 as i32, self.sizes[index].1 as i32));
                let status_count = panel_controls(&self.shell).len();
                let layout = chrome_layout::build_chrome_layout_with_config(
                    logical,
                    64,
                    76,
                    chrome_layout::ChromeLayoutConfig {
                        status_item_count: status_count,
                        sidebar_item_count: 0,
                    },
                );
                let point = (x as i32, y as i32);
                if layout.topbar.flow_field.contains(point) {
                    return Some(0);
                }
                if let Some(status) = layout
                    .topbar
                    .status_wells
                    .iter()
                    .position(|well| well.contains(point))
                {
                    return Some(status + 1);
                }
                layout
                    .topbar
                    .clock_well
                    .contains(point)
                    .then_some(status_count + 1)
            }
        }
    }

    fn action_for_control(&self, index: usize, control: usize) -> Option<DesktopAction> {
        match self.role {
            ShellRole::Dock => {
                let logical_h = self.sizes[index].1 as i32;
                let capacity = ((logical_h - 26).max(0) / 56) as usize;
                dock_controls(self.workspace_count, self.active_workspace, capacity)
                    .get(control)
                    .filter(|item| item.enabled)
                    .map(|item| item.action.clone())
            }
            ShellRole::Panel => {
                let controls = panel_controls(&self.shell);
                if control == 0 {
                    Some(launcher_control().action)
                } else if control == controls.len() + 1 {
                    Some(clock_control().action)
                } else {
                    controls
                        .get(control - 1)
                        .filter(|item| item.enabled)
                        .map(|item| item.action.clone())
                }
            }
        }
    }

    fn activate(&mut self, surface_index: usize, x: f64, y: f64) {
        let Some(control) = self.control_at(surface_index, x, y) else {
            return;
        };
        let Some(action) = self.action_for_control(surface_index, control) else {
            return;
        };
        self.pulses[surface_index] = Some(ClickPulse {
            control,
            click: (x, y),
            started: Instant::now(),
        });
        let fallback = match &action {
            DesktopAction::CreateWorkspace | DesktopAction::DeleteWorkspace => {
                Some(DesktopAction::OpenSettingsPanel {
                    panel: "workspaces".into(),
                })
            }
            DesktopAction::OpenCalendarPanel => Some(DesktopAction::LaunchApp {
                app: "gnome-calendar".into(),
            }),
            _ => None,
        };
        let response = send_desktop_request(&IpcRequest::ExecuteDesktopAction {
            action: action.clone(),
        });
        let accepted = matches!(response, Ok(IpcResponse::Ok));
        if !accepted {
            eprintln!(
                "{}: desktop action failed: {action:?}: {response:?}",
                self.role.namespace(),
            );
            if let Some(action) = fallback {
                let _ = send_desktop_request(&IpcRequest::ExecuteDesktopAction { action });
            }
        }
    }
}

impl CompositorHandler for ShellClient {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        scale: i32,
    ) {
        if let Some(index) = self.layers.iter().position(|l| l.wl_surface() == surface) {
            // wl_output.scale is integer-only. Use it only when the compositor
            // does not expose the fractional-scale + viewporter pair.
            if self.fractional_scales[index].is_some() && self.viewports[index].is_some() {
                surface.set_buffer_scale(1);
                return;
            }
            self.scales[index] = scale.max(1) as f64;
            surface.set_buffer_scale(scale.max(1));
            if self.configured[index] {
                if let Err(error) = self.draw(index, qh) {
                    self.fail("scale-change frame", error);
                }
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(index) = self.layers.iter().position(|l| l.wl_surface() == surface) {
            if let Err(error) = self.draw(index, qh) {
                self.fail("GLES frame", error);
            }
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for ShellClient {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(index) = self.layers.iter().position(|l| l == layer) {
            self.egl_surfaces[index] = None;
        }
        self.closed = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(index) = self.layers.iter().position(|l| l == layer) else {
            return;
        };
        // Older FocalDesk compositors sent an initial configure from
        // new_layer_surface, before this client had committed set_size and
        // set_anchor. Smithay consequently suggested a half-output-sized
        // rectangle (for example 1024x576), which the GLES client rendered and
        // Wayland then scaled into the 76px dock. Ignore that known-invalid
        // cross-axis size and wait for the correctly arranged configure that
        // follows the client's initial commit.
        let invalid_initial_cross_axis = !self.configured[index]
            && match self.role {
                ShellRole::Panel => configure.new_size.1 > ShellRole::Panel.preferred_size().1,
                ShellRole::Dock => configure.new_size.0 > ShellRole::Dock.preferred_size().0,
            };
        if invalid_initial_cross_axis {
            eprintln!(
                "{}: ignoring premature configure {}x{}",
                self.role.namespace(),
                configure.new_size.0,
                configure.new_size.1
            );
            return;
        }
        if configure.new_size.0 > 0 {
            self.sizes[index].0 = configure.new_size.0;
        }
        if configure.new_size.1 > 0 {
            self.sizes[index].1 = configure.new_size.1;
        }
        if let Some(viewport) = &self.viewports[index] {
            viewport.set_destination(self.sizes[index].0 as i32, self.sizes[index].1 as i32);
        }
        self.configured[index] = true;
        eprintln!(
            "{}: configured {}x{} scale {}",
            self.role.namespace(),
            self.sizes[index].0,
            self.sizes[index].1,
            self.scales[index]
        );
        if let Err(error) = self.draw(index, qh) {
            self.fail("initial GLES frame", error);
            return;
        }
        if !self.ready_reported {
            self.ready_reported = true;
            let _ = send_desktop_request(&IpcRequest::ShellReady {
                namespace: self.role.namespace().into(),
                output_count: self.layers.len(),
            });
        }
    }
}

impl OutputHandler for ShellClient {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if !self
            .layer_outputs
            .iter()
            .flatten()
            .any(|known| known == &output)
        {
            self.add_surface(qh, Some(output));
        }
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(index) = self
            .layer_outputs
            .iter()
            .position(|known| known.as_ref() == Some(&output))
        {
            self.egl_surfaces.remove(index);
            self.layers.remove(index);
            self.layer_outputs.remove(index);
            self.sizes.remove(index);
            self.scales.remove(index);
            self.fractional_scales.remove(index);
            self.viewports.remove(index);
            self.configured.remove(index);
            self.hovered.remove(index);
            self.pulses.remove(index);
        }
    }
}

impl SeatHandler for ShellClient {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointer.take();
        }
    }
}

impl PointerHandler for ShellClient {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self
                .layers
                .iter()
                .position(|layer| event.surface == *layer.wl_surface())
            else {
                continue;
            };
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let next = self.control_at(index, event.position.0, event.position.1);
                    if self.hovered[index] != next {
                        self.hovered[index] = next;
                        if self.configured[index] {
                            if let Err(error) = self.draw(index, qh) {
                                self.fail("hover frame", error);
                            }
                        }
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if self.hovered[index].take().is_some() && self.configured[index] {
                        if let Err(error) = self.draw(index, qh) {
                            self.fail("pointer-leave frame", error);
                        }
                    }
                }
                PointerEventKind::Press { .. } => {
                    self.activate(index, event.position.0, event.position.1);
                    if self.configured[index] {
                        if let Err(error) = self.draw(index, qh) {
                            self.fail("click-pulse frame", error);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for ShellClient {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, wl_surface::WlSurface> for ShellClient {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(index) = state
                .layers
                .iter()
                .position(|layer| layer.wl_surface() == surface)
            {
                state.scales[index] = (scale as f64 / 120.0).max(1.0);
                surface.set_buffer_scale(1);
                eprintln!(
                    "{}: preferred fractional scale {:.2}",
                    state.role.namespace(),
                    state.scales[index]
                );
                if state.configured[index] {
                    if let Err(error) = state.draw(index, qh) {
                        state.fail("fractional-scale frame", error);
                    }
                }
            }
        }
    }
}

impl Dispatch<WpViewporter, ()> for ShellClient {
    fn event(
        _: &mut Self,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for ShellClient {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for ShellClient {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: <wl_region::WlRegion as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(ShellClient);
delegate_output!(ShellClient);
delegate_layer!(ShellClient);
delegate_pointer!(ShellClient);
delegate_seat!(ShellClient);
delegate_registry!(ShellClient);

impl ProvidesRegistryState for ShellClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

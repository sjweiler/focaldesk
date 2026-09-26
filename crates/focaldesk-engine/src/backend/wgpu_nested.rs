//! Nested Vulkan backend built on FocalDesk's renderer boundary and wgpu.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::fonts::{FontId, TextStyle};
use anyhow::{anyhow, Context, Result};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_render::{
    FramePixelFormat, FrameRetention, FrameTransferFunction, FrameTransform, LinuxDmabuf,
    MeshVertex, PresentRenderer, PresentResult, SolidQuad, TextureColorTransform, TextureQuad,
    TexturedMesh, WgpuVulkanRenderer,
};
use focaldesk_types::OutputId;
use focaldesk_ui::atlas::IconId;
use focaldesk_ui::dialog_layout::layout_dialog;
use focaldesk_ui::egui_layer::rasterize_vulkan_paint;
use focaldesk_ui::types::UiElementKind;
use image::GenericImageView;
use smithay::backend::allocator::dmabuf::{DmabufMappingMode, DmabufSyncFlags};
use smithay::backend::allocator::{Buffer as _, Format, Fourcc, Modifier};
use smithay::backend::drm::{DrmDeviceFd, DrmNode};
use smithay::backend::renderer::element::Id as RenderElementId;
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
use smithay::desktop::{layer_map_for_output, PopupManager};
use smithay::reexports::calloop::EventLoop as CalloopEventLoop;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
use smithay::wayland::dmabuf::{get_dmabuf, DmabufFeedbackBuilder};
use smithay::wayland::drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState};
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
use crate::core::color::{SurfaceColorRenderState, TransferFunction};
use crate::core::input::{
    FlowInputEvent, FlowKeyState, FlowModifiers, FlowMouseButton, FlowScrollDelta,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
struct WgpuShellAssets {
    wallpaper_path: Option<String>,
    wallpaper: Option<WgpuWallpaper>,
    icons: HashMap<IconId, WgpuIcon>,
    unavailable_icons: HashSet<IconId>,
    font_atlas_initialized: bool,
    font_atlas_pending: Option<Vec<u8>>,
    lock_screen_was_active: bool,
}

struct WgpuSceneDesktop<'a> {
    state: &'a mut crate::core::desktop::DesktopState,
    output_id: focaldesk_types::OutputId,
}

pub(crate) struct VulkanCompositorScene {
    pub background: Vec<SolidQuad>,
    pub surfaces: Vec<TextureQuad>,
    pub overlay: Vec<SolidQuad>,
    pub overlay_after_surface: usize,
    pub foreground: Vec<SolidQuad>,
    pub foreground_after_surface: usize,
    pub egui_textures: Vec<TextureQuad>,
    pub egui_meshes: Vec<TexturedMesh>,
    pub egui_before_surface: usize,
    pub damage: Vec<[i32; 4]>,
    pub client_surface_count: usize,
    pub cursor_present: bool,
}

#[derive(Default)]
pub(crate) struct VulkanSceneBuilder {
    assets: WgpuShellAssets,
    surface_commits: HashMap<ObjectId, CommitCounter>,
    rejected_surface_buffers: HashSet<ObjectId>,
    logged_egui_mesh: bool,
}

impl VulkanSceneBuilder {
    pub fn build(
        &mut self,
        state: &mut crate::core::desktop::DesktopState,
    ) -> VulkanCompositorScene {
        let output_id = state.primary_output;
        self.build_for_output(state, output_id)
    }

    pub fn build_for_output(
        &mut self,
        state: &mut crate::core::desktop::DesktopState,
        output_id: focaldesk_types::OutputId,
    ) -> VulkanCompositorScene {
        self.build_for_output_with_cursor_policy(state, output_id, true, true, false)
    }

    pub fn build_for_output_with_cursor_policy(
        &mut self,
        state: &mut crate::core::desktop::DesktopState,
        output_id: focaldesk_types::OutputId,
        software_cursor: bool,
        cpu_dmabuf_fallback: bool,
        rasterize_egui: bool,
    ) -> VulkanCompositorScene {
        state.sync_egui_for_output(output_id, Instant::now());
        let damage = state
            .outputs
            .get(&output_id)
            .map(|output| {
                output
                    .pending_damage
                    .iter()
                    .map(|rect| [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h])
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut desktop = WgpuSceneDesktop { state, output_id };
        let (background, overlay) = collect_shell_quads(&mut desktop);
        prepare_shell_text(&mut desktop, &mut self.assets);
        let mut surfaces = Vec::new();
        append_wallpaper_quads(&mut desktop, &mut self.assets, &mut surfaces);
        append_shell_icon_quads(&desktop, &mut self.assets, &mut surfaces);
        append_topbar_text_quads(&desktop, &mut self.assets, &mut surfaces);
        let shell_surface_count = surfaces.len();
        surfaces.extend(collect_shm_surfaces(
            &desktop,
            &mut self.surface_commits,
            &mut self.rejected_surface_buffers,
            cpu_dmabuf_fallback,
        ));
        let client_surface_count = surfaces.len() - shell_surface_count;
        let overlay_after_surface = surfaces.len();
        append_notification_text_quads(&desktop, &mut self.assets, &mut surfaces);
        let foreground_after_surface = surfaces.len();
        let foreground = collect_modal_quads(&desktop);
        append_modal_text_quads(&desktop, &mut self.assets, &mut surfaces);
        let egui_before_surface = surfaces.len();
        let (egui_textures, egui_meshes, egui_raster) =
            collect_egui_meshes(&mut desktop, rasterize_egui);
        if let Some(raster) = egui_raster {
            surfaces.push(TextureQuad {
                cache_key: hashed_cache_key(6, &desktop.output_id.0),
                pixels: raster.pixels,
                width: raster.width,
                height: raster.height,
                stride: raster.width.saturating_mul(4),
                format: FramePixelFormat::Rgba8Srgb,
                color_transform: Default::default(),
                dmabuf: None,
                damage: vec![[0, 0, raster.width, raster.height]],
                destination: [
                    raster.origin[0],
                    raster.origin[1],
                    raster.width as i32,
                    raster.height as i32,
                ],
                source_uv: [0.0, 0.0, 1.0, 1.0],
                transform: FrameTransform::Normal,
                tint: [1.0; 4],
                retention: None,
            });
        }
        if !self.logged_egui_mesh && !egui_meshes.is_empty() {
            warn!(
                target: "focaldesk",
                output = ?output_id,
                meshes = egui_meshes.len(),
                texture_updates = egui_textures.len(),
                "native Vulkan egui panel pass active"
            );
            self.logged_egui_mesh = true;
        }
        if self.surface_commits.len() > 4096 {
            self.surface_commits.clear();
            self.rejected_surface_buffers.clear();
        }
        let cursor_present = software_cursor
            && append_cursor_texture_quads(
                &mut desktop,
                &mut self.surface_commits,
                &mut self.rejected_surface_buffers,
                cpu_dmabuf_fallback,
                &mut surfaces,
            );
        VulkanCompositorScene {
            background,
            surfaces,
            overlay,
            overlay_after_surface,
            foreground,
            foreground_after_surface,
            egui_textures,
            egui_meshes,
            egui_before_surface,
            damage,
            client_surface_count,
            cursor_present,
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

fn collect_egui_meshes(
    desktop: &mut WgpuSceneDesktop<'_>,
    rasterize: bool,
) -> (
    Vec<TextureQuad>,
    Vec<TexturedMesh>,
    Option<focaldesk_ui::egui_layer::VulkanEguiRaster>,
) {
    let Some(output) = desktop.state.outputs.get(&desktop.output_id) else {
        return (Vec::new(), Vec::new(), None);
    };
    let scale = output.scale_factor as f32;
    let Some(desktop_output) = desktop.state.desktop_outputs.get_mut(&desktop.output_id) else {
        return (Vec::new(), Vec::new(), None);
    };
    if !desktop_output.egui.has_open_panels() {
        return (Vec::new(), Vec::new(), None);
    }
    let paint = desktop_output.egui.take_vulkan_paint(scale);
    if rasterize {
        let raster = rasterize_vulkan_paint(&paint);
        return (Vec::new(), Vec::new(), raster);
    }
    let scoped_key = |key| hashed_cache_key(5, &(desktop.output_id.0, key));
    let textures = paint
        .textures
        .into_iter()
        .map(|texture| TextureQuad {
            cache_key: scoped_key(texture.cache_key),
            pixels: texture.pixels,
            width: texture.width,
            height: texture.height,
            stride: texture.width.saturating_mul(4),
            format: FramePixelFormat::Rgba8Srgb,
            color_transform: Default::default(),
            dmabuf: None,
            damage: texture
                .upload
                .then_some(vec![[0, 0, texture.width, texture.height]])
                .unwrap_or_default(),
            destination: [0, 0, texture.width as i32, texture.height as i32],
            source_uv: [0.0, 0.0, 1.0, 1.0],
            transform: FrameTransform::Normal,
            tint: [1.0; 4],
            retention: None,
        })
        .collect();
    let meshes = paint
        .meshes
        .into_iter()
        .map(|mesh| TexturedMesh {
            texture_key: scoped_key(mesh.texture_key),
            clip_rect: mesh.clip_rect,
            vertices: mesh
                .vertices
                .into_iter()
                .map(|vertex| MeshVertex {
                    position: vertex.position,
                    uv: vertex.uv,
                    color: vertex.color,
                })
                .collect(),
            indices: mesh.indices,
        })
        .collect();
    (textures, meshes, None)
}

struct WgpuWallpaper {
    cache_key: u64,
    width: u32,
    height: u32,
    pending_pixels: Option<Vec<u8>>,
}

struct WgpuIcon {
    cache_key: u64,
    pending_pixels: Option<Vec<u8>>,
}

fn hashed_cache_key(namespace: u8, value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    namespace.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

fn object_cache_key(id: &ObjectId) -> u64 {
    hashed_cache_key(0, id)
}

fn themed_cursor_cache_key(icon: impl Hash) -> u64 {
    hashed_cache_key(1, &icon)
}

fn frame_transform(transform: Transform) -> FrameTransform {
    match transform {
        Transform::Normal => FrameTransform::Normal,
        Transform::_90 => FrameTransform::Rotate90,
        Transform::_180 => FrameTransform::Rotate180,
        Transform::_270 => FrameTransform::Rotate270,
        Transform::Flipped => FrameTransform::Flipped,
        Transform::Flipped90 => FrameTransform::Flipped90,
        Transform::Flipped180 => FrameTransform::Flipped180,
        Transform::Flipped270 => FrameTransform::Flipped270,
    }
}

#[derive(Default)]
struct NestedVulkanApp {
    window: Option<Arc<Window>>,
    renderer: Option<WgpuVulkanRenderer>,
    desktop: Option<NestedDesktop>,
    logged_first_present: bool,
    logged_first_surface_present: bool,
    logged_first_cursor_present: bool,
    logged_dmabuf_import: bool,
    modifiers: FlowModifiers,
    scene_builder: VulkanSceneBuilder,
    surface_blocker_loop: Option<CalloopEventLoop<'static, crate::core::desktop::DesktopState>>,
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

        if let Some(event_loop) = self.surface_blocker_loop.as_mut() {
            event_loop.dispatch(Some(Duration::ZERO), &mut desktop.state)?;
        }
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.poll()?;
        }

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

        Ok(desktop.state.needs_redraw())
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
        let mut desktop = match bootstrap_compositor_core(
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
        let mut dmabuf_formats = vec![
            Format {
                code: Fourcc::Argb8888,
                modifier: Modifier::Linear,
            },
            Format {
                code: Fourcc::Xrgb8888,
                modifier: Modifier::Linear,
            },
            Format {
                code: Fourcc::Abgr8888,
                modifier: Modifier::Linear,
            },
            Format {
                code: Fourcc::Xbgr8888,
                modifier: Modifier::Linear,
            },
        ];
        for capability in renderer.dmabuf_formats() {
            let modifier = Modifier::from(capability.modifier);
            let codes = match capability.format {
                FramePixelFormat::Bgra8Srgb => [Fourcc::Argb8888, Fourcc::Xrgb8888],
                FramePixelFormat::Rgba8Srgb => [Fourcc::Abgr8888, Fourcc::Xbgr8888],
                FramePixelFormat::Bgra10Unorm => [Fourcc::Argb2101010, Fourcc::Xrgb2101010],
                FramePixelFormat::Rgba10Unorm => [Fourcc::Abgr2101010, Fourcc::Xbgr2101010],
            };
            dmabuf_formats.extend(codes.map(|code| Format { code, modifier }));
        }
        dmabuf_formats
            .sort_unstable_by_key(|format| (format.code as u32, u64::from(format.modifier)));
        dmabuf_formats.dedup();

        let drm_node = renderer
            .drm_render_node()
            .and_then(|node| DrmNode::from_dev_id(libc::makedev(node.major, node.minor)).ok());
        let dmabuf_global = if let Some(node) = drm_node {
            let feedback =
                match DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats.iter().copied())
                    .build()
                {
                    Ok(feedback) => feedback,
                    Err(error) => {
                        self.fail(event_loop, error.into());
                        return;
                    }
                };
            desktop
                .state
                .dmabuf_state
                .create_global_with_default_feedback::<crate::core::desktop::DesktopState>(
                    &desktop.display.handle(),
                    &feedback,
                )
        } else {
            desktop
                .state
                .dmabuf_state
                .create_global::<crate::core::desktop::DesktopState>(
                    &desktop.display.handle(),
                    dmabuf_formats.iter().copied(),
                )
        };
        desktop.state.dmabuf_global = Some(dmabuf_global);
        desktop.state.dmabuf_node = drm_node;
        desktop.state.wgpu_dmabuf_formats = dmabuf_formats;

        if let Some(path) = drm_node.and_then(|node| node.dev_path()) {
            if let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) {
                let owned: OwnedFd = file.into();
                let device = DrmDeviceFd::new(DeviceFd::from(owned));
                if supports_syncobj_eventfd(&device) {
                    desktop.state.drm_syncobj_state = Some(DrmSyncobjState::new::<
                        crate::core::desktop::DesktopState,
                    >(
                        &desktop.display.handle(), device
                    ));
                }
            }
        }

        let surface_blocker_loop = match CalloopEventLoop::try_new() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                self.fail(event_loop, error.into());
                return;
            }
        };
        desktop.state.surface_blocker_loop_handle = Some(surface_blocker_loop.handle());

        let dmabuf_format_count = desktop.state.wgpu_dmabuf_formats.len();
        let dmabuf_modifier_count = desktop
            .state
            .wgpu_dmabuf_formats
            .iter()
            .filter(|format| format.modifier != Modifier::Linear)
            .count();
        let explicit_sync = desktop.state.drm_syncobj_state.is_some();

        let info = renderer.info();
        info!(
            target: "focaldesk",
            adapter = %info.adapter_name,
            driver = %info.driver,
            driver_info = %info.driver_info,
            surface_format = %info.surface_format,
            direct_dmabuf_import = renderer.supports_direct_dmabuf_import(),
            dmabuf_format_count,
            dmabuf_modifier_count,
            explicit_sync,
            retained_output_damage = true,
            native_shell_geometry = true,
            wayland_display = %desktop.wayland_display,
            "initialized nested wgpu Vulkan compositor"
        );
        window.set_cursor_visible(false);
        window.request_redraw();
        self.renderer = Some(renderer);
        self.desktop = Some(desktop);
        self.surface_blocker_loop = Some(surface_blocker_loop);
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
                let scene = self
                    .desktop
                    .as_mut()
                    .map(|desktop| self.scene_builder.build(&mut desktop.state));
                let result = self
                    .renderer
                    .as_mut()
                    .context("renderer missing after nested window initialization")
                    .and_then(|renderer| {
                        let scene = scene.as_ref().context("desktop scene is unavailable")?;
                        renderer.present_frame(
                            &scene.background,
                            &scene.surfaces,
                            &scene.overlay,
                            scene.overlay_after_surface,
                            &scene.damage,
                        )
                    });
                match result {
                    Ok(PresentResult::Presented) => {
                        let client_surface_count =
                            scene.as_ref().map_or(0, |scene| scene.client_surface_count);
                        let cursor_present =
                            scene.as_ref().is_some_and(|scene| scene.cursor_present);
                        if !self.logged_dmabuf_import {
                            if let Some(renderer) = self.renderer.as_ref() {
                                let (imports, fallbacks) = renderer.dmabuf_import_stats();
                                if imports > 0 || fallbacks > 0 {
                                    focaldesk_logging::flog(format!(
                                        "wgpu DMA-BUF textures: direct_imports={imports} fallbacks={fallbacks}"
                                    ));
                                    self.logged_dmabuf_import = true;
                                }
                            }
                        }
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
        self.surface_blocker_loop = None;
        self.window = None;
        self.logged_first_present = false;
        self.logged_first_surface_present = false;
        self.logged_first_cursor_present = false;
        self.logged_dmabuf_import = false;
        self.scene_builder.clear();
    }
}

const WGPU_LABEL_STYLE: TextStyle = TextStyle {
    font: FontId::IbmPlexSansMedium,
    size_px: 15,
    letter_spacing_64: 0,
};
const WGPU_BODY_STYLE: TextStyle = TextStyle {
    font: FontId::IbmPlexSansRegular,
    size_px: 13,
    letter_spacing_64: 0,
};

fn clipped_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn font_atlas_needs_upload(
    atlas_dirty: bool,
    atlas_initialized: bool,
    lock_screen_active: bool,
    lock_screen_was_active: bool,
) -> bool {
    atlas_dirty || !atlas_initialized || (lock_screen_active && !lock_screen_was_active)
}

fn prepare_shell_text(desktop: &mut WgpuSceneDesktop<'_>, assets: &mut WgpuShellAssets) {
    let mut strings = vec![("FOCALDESK".to_string(), WGPU_LABEL_STYLE)];
    strings.extend(
        desktop
            .state
            .ui
            .elements
            .iter()
            .filter(|element| element.visible)
            .filter_map(|element| {
                element
                    .label
                    .as_deref()
                    .map(|label| (clipped_text(label, 48), WGPU_LABEL_STYLE))
            }),
    );
    for notification in desktop.state.notification_snapshots.iter().take(3) {
        strings.push((clipped_text(&notification.title, 38), WGPU_LABEL_STYLE));
        strings.push((clipped_text(&notification.body, 52), WGPU_BODY_STYLE));
    }
    if let Some(dialog) = desktop
        .state
        .active_dialog
        .and_then(|id| desktop.state.dialogs.iter().find(|dialog| dialog.id == id))
    {
        strings.push((clipped_text(&dialog.title, 64), WGPU_LABEL_STYLE));
        strings.extend(
            dialog
                .message
                .lines()
                .map(|line| (clipped_text(line, 72), WGPU_BODY_STYLE)),
        );
        strings.extend(
            dialog
                .buttons
                .iter()
                .map(|button| (clipped_text(&button.label, 24), WGPU_LABEL_STYLE)),
        );
    }
    let lock = desktop.state.lock_screen.snapshot(Instant::now());
    if lock.active {
        strings.push(("FOCALDESK LOCKED".to_string(), WGPU_LABEL_STYLE));
        strings.push((clipped_text(&lock.message, 64), WGPU_LABEL_STYLE));
        strings.push(("Show".to_string(), WGPU_LABEL_STYLE));
        strings.push(("Hide".to_string(), WGPU_LABEL_STYLE));
        if lock.password_visible {
            strings.push((clipped_text(&lock.password_text, 48), WGPU_LABEL_STYLE));
        } else if lock.password_len > 0 {
            strings.push(("*".repeat(lock.password_len.min(48)), WGPU_LABEL_STYLE));
        }
    }
    for (text, style) in strings {
        if let Err(error) = desktop.state.fonts.prepare_text(&text, style) {
            warn!(%error, "failed to prepare wgpu shell text");
        }
    }
    // The Vulkan renderers evict textures that have not been referenced for
    // 120 frames. In fullscreen sessions the shell may emit no text quads for
    // much longer than that, while this cache still considers the atlas
    // initialized. Re-upload on every unlocked -> locked transition so the
    // lock dialog never relies on an atlas the renderer may have evicted.
    let upload_atlas = font_atlas_needs_upload(
        desktop.state.fonts.atlas_dirty,
        assets.font_atlas_initialized,
        lock.active,
        assets.lock_screen_was_active,
    );
    assets.lock_screen_was_active = lock.active;
    if upload_atlas {
        let alpha = desktop.state.fonts.atlas_pixels();
        let mut rgba = Vec::with_capacity(alpha.len() * 4);
        for &value in alpha {
            rgba.extend_from_slice(&[value, value, value, value]);
        }
        assets.font_atlas_pending = Some(rgba);
        assets.font_atlas_initialized = true;
        desktop.state.fonts.clear_dirty();
    }
}

#[allow(clippy::too_many_arguments)]
fn append_text_quads(
    desktop: &WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
    text: &str,
    mut cursor_x: i32,
    baseline_y: i32,
    style: TextStyle,
    color: [f32; 4],
    scale: f64,
) {
    let (atlas_width, atlas_height) = desktop.state.fonts.atlas_size();
    for ch in text.chars() {
        if ch == ' ' {
            cursor_x += style.size_px as i32 / 2;
            continue;
        }
        let Some(glyph) = desktop.state.fonts.glyph((style.font, style.size_px, ch)) else {
            continue;
        };
        let advance = (glyph.advance + f32::from(style.letter_spacing_64) / 64.0).round() as i32;
        if glyph.w == 0 || glyph.h == 0 {
            cursor_x += advance;
            continue;
        }
        let rect = Rectangle::<i32, Logical>::from_loc_and_size(
            (
                cursor_x + glyph.xmin,
                baseline_y - glyph.ymin - glyph.h as i32,
            ),
            (glyph.w as i32, glyph.h as i32),
        )
        .to_physical_precise_round(Scale::from(scale));
        let upload_pending = assets.font_atlas_pending.is_some();
        output.push(TextureQuad {
            cache_key: hashed_cache_key(4, &"font-atlas"),
            pixels: assets.font_atlas_pending.take().unwrap_or_default(),
            width: atlas_width,
            height: atlas_height,
            stride: atlas_width.saturating_mul(4),
            format: FramePixelFormat::Rgba8Srgb,
            color_transform: Default::default(),
            dmabuf: None,
            damage: if upload_pending {
                vec![[0, 0, atlas_width, atlas_height]]
            } else {
                Vec::new()
            },
            destination: [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
            source_uv: [
                glyph.atlas_x as f32 / atlas_width as f32,
                glyph.atlas_y as f32 / atlas_height as f32,
                (glyph.atlas_x + glyph.w) as f32 / atlas_width as f32,
                (glyph.atlas_y + glyph.h) as f32 / atlas_height as f32,
            ],
            transform: FrameTransform::Normal,
            tint: premultiplied(color),
            retention: None,
        });
        cursor_x += advance;
    }
}

fn append_topbar_text_quads(
    desktop: &WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
) {
    let output_id = desktop.output_id;
    let Some(output_state) = desktop.state.outputs.get(&output_id) else {
        return;
    };
    let visibility = desktop
        .state
        .internal_chrome_visibility_for_output(output_id, Instant::now());
    if !visibility.topbar {
        return;
    }
    let Some(layout) = desktop.state.chrome_layout_for_output(output_id) else {
        return;
    };
    let theme = desktop.state.theme.active_theme();
    append_text_quads(
        desktop,
        assets,
        output,
        "FOCALDESK",
        layout.topbar.title.loc.x + 12,
        layout.topbar.title.loc.y + layout.topbar.title.size.h / 2 + 5,
        WGPU_LABEL_STYLE,
        theme.text.title,
        output_state.scale_factor,
    );
    for element in desktop
        .state
        .ui
        .elements
        .iter()
        .filter(|element| element.visible)
    {
        let Some(label) = element.label.as_deref() else {
            continue;
        };
        append_text_quads(
            desktop,
            assets,
            output,
            &clipped_text(label, 48),
            element.bounds.x + 6,
            element.bounds.y + element.bounds.h / 2 + 5,
            WGPU_LABEL_STYLE,
            theme.text.normal,
            output_state.scale_factor,
        );
    }
}

fn append_notification_text_quads(
    desktop: &WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
) {
    if desktop.state.lock_screen.active && desktop.state.privacy.hide_lock_screen_notifications {
        return;
    }
    let output_id = desktop.output_id;
    let Some(output_state) = desktop.state.outputs.get(&output_id) else {
        return;
    };
    let Some(layout) = desktop.state.chrome_layout_for_output(output_id) else {
        return;
    };
    let theme = desktop.state.theme.active_theme();
    let card_width = 320.min((output_state.logical_size.w - 24).max(1));
    let x = (output_state.logical_size.w - card_width - 12).max(0) + 14;
    let mut y = layout.topbar.outer.size.h + 12;
    for notification in desktop.state.notification_snapshots.iter().take(3) {
        append_text_quads(
            desktop,
            assets,
            output,
            &clipped_text(&notification.title, 38),
            x,
            y + 25,
            WGPU_LABEL_STYLE,
            theme.text.title,
            output_state.scale_factor,
        );
        append_text_quads(
            desktop,
            assets,
            output,
            &clipped_text(&notification.body, 52),
            x,
            y + 52,
            WGPU_BODY_STYLE,
            theme.text.normal,
            output_state.scale_factor,
        );
        y += 92;
    }
}

fn append_modal_text_quads(
    desktop: &WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
) {
    let Some(output_state) = desktop.state.outputs.get(&desktop.output_id) else {
        return;
    };
    let scale = output_state.scale_factor;
    let screen = Rectangle::<i32, Logical>::from_loc_and_size((0, 0), output_state.logical_size);
    let theme = desktop.state.theme.active_theme();

    if let Some(dialog) = desktop
        .state
        .active_dialog
        .and_then(|id| desktop.state.dialogs.iter().find(|dialog| dialog.id == id))
        .filter(|dialog| dialog.owner_output == desktop.output_id)
    {
        let layout = layout_dialog(dialog, screen);
        append_text_quads(
            desktop,
            assets,
            output,
            &clipped_text(&dialog.title, 64),
            layout.title_rect.loc.x,
            layout.title_rect.loc.y + layout.title_rect.size.h - 8,
            WGPU_LABEL_STYLE,
            theme.dialog.title_color,
            scale,
        );
        let mut y = layout.message_rect.loc.y + 20;
        for line in dialog.message.lines() {
            append_text_quads(
                desktop,
                assets,
                output,
                &clipped_text(line, 72),
                layout.message_rect.loc.x,
                y,
                WGPU_BODY_STYLE,
                theme.dialog.text_color,
                scale,
            );
            y += 22;
        }
        for (index, rect) in &layout.button_rects {
            if let Some(button) = dialog.buttons.get(*index) {
                append_text_quads(
                    desktop,
                    assets,
                    output,
                    &clipped_text(&button.label, 24),
                    rect.loc.x + 12,
                    rect.loc.y + rect.size.h / 2 + 5,
                    WGPU_LABEL_STYLE,
                    theme.dialog.text_color,
                    scale,
                );
            }
        }
    }

    let lock = desktop.state.lock_screen.snapshot(Instant::now());
    if !lock.active {
        return;
    }
    let panel_w = screen.size.w.min(460).max(1);
    let panel_h = screen.size.h.min(190).max(1);
    let panel_x = (screen.size.w - panel_w) / 2;
    let panel_y = (screen.size.h - panel_h) / 2;
    append_text_quads(
        desktop,
        assets,
        output,
        "FOCALDESK LOCKED",
        panel_x + 28,
        panel_y + 48,
        WGPU_LABEL_STYLE,
        theme.text.title,
        scale,
    );
    let password = if lock.password_visible {
        clipped_text(&lock.password_text, 48)
    } else {
        "*".repeat(lock.password_len.min(48))
    };
    append_text_quads(
        desktop,
        assets,
        output,
        &password,
        panel_x + 46,
        panel_y + 103,
        WGPU_LABEL_STYLE,
        theme.text.normal,
        scale,
    );
    append_text_quads(
        desktop,
        assets,
        output,
        if lock.password_visible {
            "Hide"
        } else {
            "Show"
        },
        panel_x + panel_w - 94,
        panel_y + 103,
        WGPU_LABEL_STYLE,
        theme.text.normal,
        scale,
    );
    append_text_quads(
        desktop,
        assets,
        output,
        if lock.authenticating {
            "Authenticating"
        } else {
            lock.message.as_str()
        },
        panel_x + 28,
        panel_y + 150,
        WGPU_LABEL_STYLE,
        theme.text.dim,
        scale,
    );
}

fn collect_modal_quads(desktop: &WgpuSceneDesktop<'_>) -> Vec<SolidQuad> {
    let Some(output) = desktop.state.outputs.get(&desktop.output_id) else {
        return Vec::new();
    };
    let scale = output.scale_factor;
    let screen = Rectangle::<i32, Logical>::from_loc_and_size((0, 0), output.logical_size);
    let theme = desktop.state.theme.active_theme();
    let mut quads = Vec::new();
    if let Some(dialog) = desktop
        .state
        .active_dialog
        .and_then(|id| desktop.state.dialogs.iter().find(|dialog| dialog.id == id))
    {
        quads.push(solid_quad(screen, scale, [0.0, 0.0, 0.0, 0.45]));
        if dialog.owner_output == desktop.output_id {
            let layout = layout_dialog(dialog, screen);
            quads.push(rounded_solid_quad(
                layout.bounds,
                scale,
                theme.dialog.panel_color,
                8.0,
            ));
            for (_, rect) in &layout.button_rects {
                quads.push(rounded_solid_quad(
                    *rect,
                    scale,
                    theme.dialog.button_color,
                    4.0,
                ));
            }
        }
    }

    let lock = desktop.state.lock_screen.snapshot(Instant::now());
    if lock.active {
        quads.push(solid_quad(screen, scale, [0.005, 0.008, 0.014, 0.92]));
        let panel_w = screen.size.w.min(460).max(1);
        let panel_h = screen.size.h.min(190).max(1);
        let panel_x = (screen.size.w - panel_w) / 2;
        let panel_y = (screen.size.h - panel_h) / 2;
        let panel = Rectangle::from_loc_and_size((panel_x, panel_y), (panel_w, panel_h));
        let mut panel_color = theme.chrome.panel_color;
        panel_color[3] = 0.96;
        quads.push(rounded_solid_quad(panel, scale, panel_color, 12.0));
        quads.push(rounded_solid_quad(
            Rectangle::from_loc_and_size((panel_x + 28, panel_y + 72), (panel_w - 56, 48)),
            scale,
            [0.02, 0.03, 0.05, 0.96],
            8.0,
        ));
        quads.push(rounded_solid_quad(
            Rectangle::from_loc_and_size((panel_x + panel_w - 110, panel_y + 80), (68, 32)),
            scale,
            [0.08, 0.11, 0.16, 0.96],
            6.0,
        ));
    }
    quads
}

fn append_wallpaper_quads(
    desktop: &mut WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
) {
    let output_id = desktop.output_id;
    let theme = desktop.state.theme.active_theme();
    let path = theme.wallpaper.path.clone();
    if assets.wallpaper_path != path {
        assets.wallpaper_path.clone_from(&path);
        assets.wallpaper = path.as_deref().and_then(|path| {
            let image = match image::open(path) {
                Ok(image) => image,
                Err(error) => {
                    warn!(%path, %error, "failed to decode wgpu wallpaper");
                    return None;
                }
            };
            let (width, height) = image.dimensions();
            Some(WgpuWallpaper {
                cache_key: hashed_cache_key(2, &path),
                width,
                height,
                pending_pixels: Some(image.to_rgba8().into_raw()),
            })
        });
    }
    let Some(asset) = assets.wallpaper.as_mut() else {
        return;
    };
    let Some(layout) = desktop.state.chrome_layout_for_output(output_id) else {
        return;
    };
    let Some(output_state) = desktop.state.outputs.get(&output_id) else {
        return;
    };
    let mode = match theme.wallpaper.fit {
        focaldesk_themes::ThemeWallpaperFit::Fill => crate::core::wallpaper::WallpaperMode::Fill,
        focaldesk_themes::ThemeWallpaperFit::Fit => crate::core::wallpaper::WallpaperMode::Fit,
        focaldesk_themes::ThemeWallpaperFit::Stretch => {
            crate::core::wallpaper::WallpaperMode::Stretch
        }
        focaldesk_themes::ThemeWallpaperFit::Center => {
            crate::core::wallpaper::WallpaperMode::Center
        }
        focaldesk_themes::ThemeWallpaperFit::Tile => crate::core::wallpaper::WallpaperMode::Tile,
    };
    let work = layout.work_area.recess;
    let blits = crate::core::wallpaper::compute_wallpaper_blits(
        crate::core::wallpaper::SizeI {
            w: asset.width as i32,
            h: asset.height as i32,
        },
        crate::core::wallpaper::RectI {
            x: work.loc.x,
            y: work.loc.y,
            w: work.size.w,
            h: work.size.h,
        },
        mode,
    );
    let tint = theme.wallpaper.tint_color;
    let dim = (1.0 - theme.wallpaper.dim).clamp(0.0, 1.0);
    let tint = [
        dim * ((1.0 - tint[3]) + tint[0] * tint[3]),
        dim * ((1.0 - tint[3]) + tint[1] * tint[3]),
        dim * ((1.0 - tint[3]) + tint[2] * tint[3]),
        1.0,
    ];
    let upload_pending = asset.pending_pixels.is_some();
    for (index, blit) in blits.into_iter().enumerate() {
        let destination = Rectangle::<i32, Logical>::from_loc_and_size(
            (blit.dst.x, blit.dst.y),
            (blit.dst.w, blit.dst.h),
        )
        .to_physical_precise_round(output_state.scale);
        output.push(TextureQuad {
            cache_key: asset.cache_key,
            pixels: if index == 0 {
                asset.pending_pixels.take().unwrap_or_default()
            } else {
                Vec::new()
            },
            width: asset.width,
            height: asset.height,
            stride: asset.width.saturating_mul(4),
            format: FramePixelFormat::Rgba8Srgb,
            color_transform: Default::default(),
            dmabuf: None,
            damage: if index == 0 && upload_pending {
                vec![[0, 0, asset.width, asset.height]]
            } else {
                Vec::new()
            },
            destination: [
                destination.loc.x,
                destination.loc.y,
                destination.size.w,
                destination.size.h,
            ],
            source_uv: [blit.uv.u0, blit.uv.v0, blit.uv.u1, blit.uv.v1],
            transform: FrameTransform::Normal,
            tint: premultiplied(tint),
            retention: None,
        });
    }
}

fn append_shell_icon_quads(
    desktop: &WgpuSceneDesktop<'_>,
    assets: &mut WgpuShellAssets,
    output: &mut Vec<TextureQuad>,
) {
    let output_id = desktop.output_id;
    let Some(output_state) = desktop.state.outputs.get(&output_id) else {
        return;
    };
    let scale = output_state.scale_factor;
    let visibility = desktop
        .state
        .internal_chrome_visibility_for_output(output_id, Instant::now());
    let theme = desktop.state.theme.active_theme();
    for element in desktop.state.ui.elements.iter().filter(|element| {
        element.visible
            && match element.kind {
                UiElementKind::SidebarButton | UiElementKind::WorkspaceSlot => visibility.sidebar(),
                UiElementKind::TopbarIndicator
                | UiElementKind::TopbarButton
                | UiElementKind::TopbarFlowField
                | UiElementKind::Clock
                | UiElementKind::OutputLabel => visibility.topbar,
            }
    }) {
        let Some(icon) = element.icon else {
            continue;
        };
        if !assets.icons.contains_key(&icon) && !assets.unavailable_icons.contains(&icon) {
            match focaldesk_ui::atlas::rasterize_icon_rgba(icon, 48) {
                Ok(mut pixels) => {
                    for pixel in pixels.as_chunks_mut::<4>().0 {
                        let alpha = u16::from(pixel[3]);
                        pixel[0] = (u16::from(pixel[0]) * alpha / 255) as u8;
                        pixel[1] = (u16::from(pixel[1]) * alpha / 255) as u8;
                        pixel[2] = (u16::from(pixel[2]) * alpha / 255) as u8;
                    }
                    assets.icons.insert(
                        icon,
                        WgpuIcon {
                            cache_key: hashed_cache_key(3, &icon),
                            pending_pixels: Some(pixels),
                        },
                    );
                }
                Err(error) => {
                    trace!(?icon, %error, "wgpu icon has no raster source");
                    assets.unavailable_icons.insert(icon);
                    continue;
                }
            }
        }
        let Some(asset) = assets.icons.get_mut(&icon) else {
            continue;
        };
        let state_scale = if element.active {
            element.press_scale
        } else if element.hovered || element.selected {
            element.hover_scale
        } else {
            1.0
        };
        let icon_size = ((element.bounds.w.min(element.bounds.h) - 10).max(1) as f32 * state_scale)
            .round() as i32;
        let rect = Rectangle::<i32, Logical>::from_loc_and_size(
            (
                element.bounds.x + (element.bounds.w - icon_size) / 2,
                element.bounds.y + (element.bounds.h - icon_size) / 2,
            ),
            (icon_size, icon_size),
        )
        .to_physical_precise_round(Scale::from(scale));
        let upload_pending = asset.pending_pixels.is_some();
        let tint = if !element.enabled {
            theme.icons.disabled
        } else if element.active || element.selected {
            theme.icons.active
        } else if element.hovered {
            theme.icons.hover
        } else {
            theme.icons.inactive
        };
        output.push(TextureQuad {
            cache_key: asset.cache_key,
            pixels: asset.pending_pixels.take().unwrap_or_default(),
            width: 48,
            height: 48,
            stride: 48 * 4,
            format: FramePixelFormat::Rgba8Srgb,
            color_transform: Default::default(),
            dmabuf: None,
            damage: if upload_pending {
                vec![[0, 0, 48, 48]]
            } else {
                Vec::new()
            },
            destination: [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
            source_uv: [0.0, 0.0, 1.0, 1.0],
            transform: FrameTransform::Normal,
            tint: premultiplied(tint),
            retention: None,
        });
    }
}

fn premultiplied(mut color: [f32; 4]) -> [f32; 4] {
    color[0] *= color[3];
    color[1] *= color[3];
    color[2] *= color[3];
    color
}

fn solid_quad(rect: Rectangle<i32, Logical>, scale: f64, color: [f32; 4]) -> SolidQuad {
    let rect = rect.to_physical_precise_round(Scale::from(scale));
    SolidQuad {
        destination: [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
        color: premultiplied(color),
        corner_radius: 0.0,
    }
}

fn rounded_solid_quad(
    rect: Rectangle<i32, Logical>,
    scale: f64,
    color: [f32; 4],
    radius: f32,
) -> SolidQuad {
    let mut quad = solid_quad(rect, scale, color);
    quad.corner_radius = radius * scale as f32;
    quad
}

/// Build the compositor-native shell geometry used while the GLES chrome is
/// being ported. Client content remains between the background and overlay
/// lists, so notification cards stay above application windows.
fn collect_shell_quads(desktop: &mut WgpuSceneDesktop<'_>) -> (Vec<SolidQuad>, Vec<SolidQuad>) {
    let output_id = desktop.output_id;
    let Some(layout) = desktop.state.rebuild_ui_tree_for_output(output_id) else {
        return (Vec::new(), Vec::new());
    };
    let Some(output) = desktop.state.outputs.get(&output_id) else {
        return (Vec::new(), Vec::new());
    };
    let scale = output.scale_factor;
    let buffer_size = output.physical_size;
    let visibility = desktop
        .state
        .internal_chrome_visibility_for_output(output_id, Instant::now());
    let theme = desktop.state.theme.active_theme();
    let chrome = theme.chrome;
    let mut background = vec![SolidQuad {
        destination: [0, 0, buffer_size.w, buffer_size.h],
        color: premultiplied(theme.background.color),
        corner_radius: 0.0,
    }];

    for (rect, color) in [
        (layout.work_area.outer, chrome.bg_color),
        (layout.work_area.inner_frame, chrome.trim_color),
        (layout.work_area.recess, theme.background.color),
    ] {
        background.push(solid_quad(rect, scale, color));
    }
    if let Some(trim) = layout.work_area.trim {
        background.push(solid_quad(trim, scale, chrome.accent_color));
    }

    if visibility.topbar {
        background.push(rounded_solid_quad(
            layout.topbar.outer,
            scale,
            chrome.bg_color,
            chrome.corner_radius,
        ));
        for (rect, color) in [
            (layout.topbar.inner, chrome.panel_color),
            (layout.topbar.title, chrome.bg_color),
            (layout.topbar.trim, chrome.trim_color),
            (layout.topbar.ai_button, chrome.trim_color),
            (layout.topbar.clock_well, chrome.bg_color),
        ] {
            background.push(solid_quad(rect, scale, color));
        }
        for &well in &layout.topbar.status_wells {
            background.push(solid_quad(well, scale, chrome.bg_color));
        }
        if let Some(light) = layout.topbar.light {
            background.push(solid_quad(light, scale, chrome.accent_color));
        }
    }

    if visibility.sidebar() {
        background.push(rounded_solid_quad(
            layout.sidebar.outer,
            scale,
            chrome.bg_color,
            focaldesk_ui::chrome_layout::SIDEBAR_CORNER_RADIUS,
        ));
        background.push(solid_quad(layout.sidebar.inner, scale, chrome.panel_color));
        for slot in &layout.sidebar.slots {
            background.push(solid_quad(slot.outer, scale, chrome.trim_color));
            background.push(solid_quad(slot.inner, scale, chrome.panel_color));
            background.push(solid_quad(slot.icon_well, scale, chrome.bg_color));
        }
        if let Some(light) = layout.sidebar.light {
            background.push(solid_quad(light, scale, chrome.accent_color));
        }
        for &cap in &layout.sidebar.caps {
            background.push(solid_quad(cap, scale, chrome.trim_color));
        }
    }

    let mut overlay = Vec::new();
    if !(desktop.state.lock_screen.active && desktop.state.privacy.hide_lock_screen_notifications) {
        let card_width = 320.min((output.logical_size.w - 24).max(1));
        let card_height = 82;
        let x = (output.logical_size.w - card_width - 12).max(0);
        let mut y = layout.topbar.outer.size.h + 12;
        for _ in desktop.state.notification_snapshots.iter().take(3) {
            let card = Rectangle::from_loc_and_size((x, y), (card_width, card_height));
            let stripe = Rectangle::from_loc_and_size((x, y), (4, card_height));
            let mut card_color = theme.dialog.panel_color;
            card_color[3] = card_color[3].min(0.96);
            overlay.push(rounded_solid_quad(card, scale, card_color, 10.0));
            overlay.push(solid_quad(stripe, scale, chrome.accent_color));
            y += card_height + 10;
        }
    }

    (background, overlay)
}

fn append_cursor_texture_quads(
    desktop: &mut WgpuSceneDesktop<'_>,
    surface_commits: &mut HashMap<ObjectId, CommitCounter>,
    rejected_surface_buffers: &mut HashSet<ObjectId>,
    cpu_dmabuf_fallback: bool,
    output: &mut Vec<TextureQuad>,
) -> bool {
    if !desktop.state.cursor_manager.visible()
        || !desktop.state.output_contains_pointer(desktop.output_id)
    {
        return false;
    }
    let (pointer_x, pointer_y) = desktop.state.cursor_manager.position();
    let Some(output_state) = desktop.state.outputs.get(&desktop.output_id) else {
        return false;
    };
    let scale = output_state.scale_factor;
    let output_origin = output_state.logical_origin;

    if let Some(cursor_surface) = desktop.state.render.sw_cursor_surface.clone() {
        let (hotspot_x, hotspot_y) = desktop.state.render.sw_cursor_hotspot;
        let origin = Point::<i32, Logical>::from((
            (pointer_x - f64::from(output_origin.x)).round() as i32 - hotspot_x,
            (pointer_y - f64::from(output_origin.y)).round() as i32 - hotspot_y,
        ));
        let start = output.len();
        let mut cursor_surfaces = Vec::new();
        collect_shm_surface_tree(
            &cursor_surface,
            origin,
            scale,
            surface_commits,
            rejected_surface_buffers,
            cpu_dmabuf_fallback,
            &desktop.state.surface_colors,
            &mut cursor_surfaces,
        );
        output.extend(cursor_surfaces.into_iter().rev());
        return output.len() > start;
    }

    let cursor_icon = desktop.state.cursor_manager.current_flow_icon();
    let Ok(image) = desktop.state.cursor_manager.current_image() else {
        return false;
    };
    let x =
        ((pointer_x - f64::from(output_origin.x)) * scale).round() as i32 - image.hotspot_x as i32;
    let y =
        ((pointer_y - f64::from(output_origin.y)) * scale).round() as i32 - image.hotspot_y as i32;

    output.push(TextureQuad {
        cache_key: themed_cursor_cache_key(cursor_icon),
        pixels: image.rgba.clone(),
        width: image.width,
        height: image.height,
        stride: image.width.saturating_mul(4),
        format: FramePixelFormat::Rgba8Srgb,
        color_transform: Default::default(),
        dmabuf: None,
        damage: Vec::new(),
        destination: [x, y, image.width as i32, image.height as i32],
        source_uv: [0.0, 0.0, 1.0, 1.0],
        transform: FrameTransform::Normal,
        tint: [1.0; 4],
        retention: None,
    });
    true
}

fn collect_shm_surfaces(
    desktop: &WgpuSceneDesktop<'_>,
    surface_commits: &mut HashMap<ObjectId, CommitCounter>,
    rejected_surface_buffers: &mut HashSet<ObjectId>,
    cpu_dmabuf_fallback: bool,
) -> Vec<TextureQuad> {
    let output_state = desktop.state.outputs.get(&desktop.output_id);
    let scale = output_state
        .map(|output| output.scale_factor)
        .unwrap_or(1.0);
    let visible_windows: HashSet<_> = output_state
        .map(|output| {
            desktop
                .state
                .windows
                .iter()
                .filter(|window| {
                    crate::core::render::managed_window_is_visible_on_output(
                        &desktop.state.space,
                        window,
                        output.active_workspace,
                        &output.handle,
                    )
                })
                .map(|window| &window.window)
                .collect()
        })
        .unwrap_or_default();

    let mut output = Vec::new();
    if let Some(output_state) = output_state {
        collect_shm_layers(
            &output_state.handle,
            &[WlrLayer::Background, WlrLayer::Bottom],
            scale,
            surface_commits,
            rejected_surface_buffers,
            cpu_dmabuf_fallback,
            &desktop.state.surface_colors,
            &mut output,
        );
    }
    for window in desktop.state.space.elements() {
        if !visible_windows.contains(window) {
            continue;
        }
        let Some(window_location) = desktop.state.space.element_location(window) else {
            continue;
        };
        let Some(root) = window.wl_surface() else {
            continue;
        };
        let output_origin = output_state
            .map(|output| output.logical_origin)
            .unwrap_or_default();
        let window_origin = window_location - window.geometry().loc - output_origin;

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
                surface_commits,
                rejected_surface_buffers,
                cpu_dmabuf_fallback,
                &desktop.state.surface_colors,
                &mut window_surfaces,
            );
        }
        collect_shm_surface_tree(
            &root,
            window_origin,
            scale,
            surface_commits,
            rejected_surface_buffers,
            cpu_dmabuf_fallback,
            &desktop.state.surface_colors,
            &mut window_surfaces,
        );
        output.extend(window_surfaces.into_iter().rev());
    }
    if let Some(output_state) = output_state {
        collect_shm_layers(
            &output_state.handle,
            &[WlrLayer::Top, WlrLayer::Overlay],
            scale,
            surface_commits,
            rejected_surface_buffers,
            cpu_dmabuf_fallback,
            &desktop.state.surface_colors,
            &mut output,
        );
    }
    output
}

fn collect_shm_layers(
    output_handle: &smithay::output::Output,
    layer_kinds: &[WlrLayer],
    scale: f64,
    surface_commits: &mut HashMap<ObjectId, CommitCounter>,
    rejected_surface_buffers: &mut HashSet<ObjectId>,
    cpu_dmabuf_fallback: bool,
    surface_colors: &HashMap<RenderElementId, SurfaceColorRenderState>,
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
                    surface_commits,
                    rejected_surface_buffers,
                    cpu_dmabuf_fallback,
                    surface_colors,
                    &mut layer_surfaces,
                );
            }
            collect_shm_surface_tree(
                root,
                geometry.loc,
                scale,
                surface_commits,
                rejected_surface_buffers,
                cpu_dmabuf_fallback,
                surface_colors,
                &mut layer_surfaces,
            );
            output.extend(layer_surfaces.into_iter().rev());
        }
    }
}

fn collect_shm_surface_tree(
    root: &WlSurface,
    origin: Point<i32, Logical>,
    scale: f64,
    surface_commits: &mut HashMap<ObjectId, CommitCounter>,
    rejected_surface_buffers: &mut HashSet<ObjectId>,
    cpu_dmabuf_fallback: bool,
    surface_colors: &HashMap<RenderElementId, SurfaceColorRenderState>,
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
        |surface, states, location| {
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
            let transform = frame_transform(renderer_state.buffer_transform());
            if buffer_size.w <= 0 || buffer_size.h <= 0 {
                return;
            }
            // Track damage age per wl_buffer, not per wl_surface. With
            // double-buffering, a returning buffer needs all damage accumulated
            // since that particular buffer was last uploaded.
            let buffer_id = buffer.id();
            let current_commit = renderer_state.current_commit();
            let damage = renderer_state
                .damage_since(surface_commits.get(&buffer_id).copied())
                .into_iter()
                .map(|rect| [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h])
                .collect::<Vec<_>>();
            let surface_location = *location + view.offset;
            let color_transform = surface_colors
                .get(&RenderElementId::from_wayland_resource(surface))
                .copied()
                .map(texture_color_transform)
                .unwrap_or_default();
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
            let frame = shm_surface_frame(
                &buffer,
                destination,
                source_uv,
                transform,
                damage.clone(),
                color_transform,
            )
            .or_else(|| {
                dmabuf_surface_frame(
                    &buffer,
                    destination,
                    source_uv,
                    transform,
                    damage,
                    color_transform,
                    cpu_dmabuf_fallback,
                )
            });
            if let Some(frame) = frame {
                output.push(frame);
                surface_commits.insert(buffer_id, current_commit);
            } else if rejected_surface_buffers.insert(buffer_id.clone()) {
                let dmabuf_format = get_dmabuf(&buffer).ok().map(|dmabuf| dmabuf.format());
                warn!(
                    target: "focaldesk",
                    buffer = ?buffer_id,
                    ?dmabuf_format,
                    "raw Vulkan rejected a committed client buffer"
                );
            }
        },
        |_, _, _| true,
    );
}

fn texture_color_transform(color: SurfaceColorRenderState) -> TextureColorTransform {
    let transfer = match color.description.transfer {
        TransferFunction::Srgb => FrameTransferFunction::Srgb,
        TransferFunction::Bt1886 => FrameTransferFunction::Bt1886,
        TransferFunction::Gamma22 => FrameTransferFunction::Gamma22,
        TransferFunction::Linear => FrameTransferFunction::Linear,
        TransferFunction::St2084Pq => FrameTransferFunction::St2084Pq,
        TransferFunction::Hlg => FrameTransferFunction::Hlg,
        TransferFunction::SrgbHdr => FrameTransferFunction::ExtendedSrgb,
    };
    TextureColorTransform {
        transfer,
        client_to_scene: color.client_to_scene,
        reference_white_nits: color.description.reference_white_nits.max(1.0),
        source_peak_nits: color.source_peak_nits.max(1.0),
        linear_to_scene_scale: color.description.linear_to_scene_scale(),
        source_bits: color.src_bits,
    }
}

fn dmabuf_surface_frame(
    buffer: &smithay::backend::renderer::utils::Buffer,
    destination: [i32; 4],
    mut source_uv: [f32; 4],
    transform: FrameTransform,
    damage: Vec<[i32; 4]>,
    color_transform: TextureColorTransform,
    cpu_fallback: bool,
) -> Option<TextureQuad> {
    let dmabuf = get_dmabuf(buffer).ok()?;
    let size = dmabuf.size();
    let format = dmabuf.format();
    if !matches!(
        format.code,
        Fourcc::Argb8888
            | Fourcc::Xrgb8888
            | Fourcc::Abgr8888
            | Fourcc::Xbgr8888
            | Fourcc::Argb2101010
            | Fourcc::Xrgb2101010
            | Fourcc::Abgr2101010
            | Fourcc::Xbgr2101010
    ) || size.w <= 0
        || size.h <= 0
    {
        return None;
    }

    let strides = dmabuf.strides().collect::<Vec<_>>();
    let offsets = dmabuf.offsets().collect::<Vec<_>>();
    let handles = dmabuf
        .handles()
        .map(|fd| fd.try_clone_to_owned().ok().map(Arc::new))
        .collect::<Option<Vec<_>>>()?;
    if handles.is_empty() || handles.len() != strides.len() || handles.len() != offsets.len() {
        return None;
    }
    let stride = strides[0];
    if stride < (size.w as u32).saturating_mul(4) {
        return None;
    }
    let mut pixels = if cpu_fallback && format.modifier == Modifier::Linear {
        let byte_len = usize::try_from(stride)
            .ok()?
            .checked_mul(usize::try_from(size.h).ok()?)?;
        dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::READ)
            .ok()?;
        let pixels = (|| {
            let mapping = dmabuf.map_plane(0, DmabufMappingMode::READ).ok()?;
            if byte_len > mapping.length() {
                return None;
            }
            // SAFETY: the mapping is readable for `mapping.length()` bytes and
            // remains alive until the owned copy has completed.
            let source =
                unsafe { std::slice::from_raw_parts(mapping.ptr().cast::<u8>(), byte_len) };
            Some(source.to_vec())
        })();
        let sync_ended = dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::READ)
            .is_ok();
        pixels.filter(|_| sync_ended)?
    } else {
        Vec::new()
    };
    #[cfg(target_endian = "little")]
    if matches!(format.code, Fourcc::Xrgb8888 | Fourcc::Xbgr8888) {
        for row in pixels.chunks_exact_mut(stride as usize) {
            for pixel in row[..size.w as usize * 4].as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }
    }

    #[cfg(target_endian = "big")]
    for row in pixels.chunks_exact_mut(stride as usize) {
        for pixel in row[..size.w as usize * 4].as_chunks_mut::<4>().0 {
            let [a, r, g, b] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let alpha = if matches!(format.code, Fourcc::Xrgb8888 | Fourcc::Xbgr8888) {
                255
            } else {
                a
            };
            match format.code {
                Fourcc::Argb8888 | Fourcc::Xrgb8888 => {
                    pixel.copy_from_slice(&[b, g, r, alpha]);
                }
                Fourcc::Abgr8888 | Fourcc::Xbgr8888 => {
                    pixel.copy_from_slice(&[r, g, b, alpha]);
                }
                _ => unreachable!("format was validated above"),
            }
        }
    }

    if dmabuf.y_inverted() {
        source_uv.swap(1, 3);
    }
    let damage = damage
        .into_iter()
        .filter_map(|[x, y, width, height]| {
            let x = x.max(0) as u32;
            let y = y.max(0) as u32;
            let width = width.max(0) as u32;
            let height = height.max(0) as u32;
            (x < size.w as u32 && y < size.h as u32 && width > 0 && height > 0)
                .then_some([x, y, width, height])
        })
        .collect();

    Some(TextureQuad {
        cache_key: object_cache_key(&buffer.id()),
        pixels,
        width: size.w as u32,
        height: size.h as u32,
        stride,
        format: match format.code {
            Fourcc::Argb8888 | Fourcc::Xrgb8888 => FramePixelFormat::Bgra8Srgb,
            Fourcc::Abgr8888 | Fourcc::Xbgr8888 => FramePixelFormat::Rgba8Srgb,
            Fourcc::Argb2101010 | Fourcc::Xrgb2101010 => FramePixelFormat::Bgra10Unorm,
            Fourcc::Abgr2101010 | Fourcc::Xbgr2101010 => FramePixelFormat::Rgba10Unorm,
            _ => unreachable!("format was validated above"),
        },
        color_transform,
        dmabuf: Some(LinuxDmabuf {
            planes: handles,
            fourcc: format.code as u32,
            modifier: format.modifier.into(),
            offsets,
            strides,
        }),
        damage,
        destination,
        source_uv,
        transform,
        tint: [1.0; 4],
        retention: Some(FrameRetention::new(buffer.clone())),
    })
}

fn shm_surface_frame(
    buffer: &smithay::backend::renderer::utils::Buffer,
    destination: [i32; 4],
    source_uv: [f32; 4],
    transform: FrameTransform,
    damage: Vec<[i32; 4]>,
    color_transform: TextureColorTransform,
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
                for pixel in row[..data.width as usize * 4].as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
        }

        #[cfg(target_endian = "big")]
        for row in pixels.chunks_exact_mut(stride) {
            for pixel in row[..data.width as usize * 4].as_chunks_mut::<4>().0 {
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

        let damage = damage
            .into_iter()
            .filter_map(|[x, y, width, height]| {
                let x = x.max(0) as u32;
                let y = y.max(0) as u32;
                let width = width.max(0) as u32;
                let height = height.max(0) as u32;
                (x < data.width as u32 && y < data.height as u32 && width > 0 && height > 0)
                    .then_some([x, y, width, height])
            })
            .collect();
        Some(TextureQuad {
            cache_key: object_cache_key(&buffer.id()),
            pixels,
            width: data.width as u32,
            height: data.height as u32,
            stride: data.stride as u32,
            format: FramePixelFormat::Bgra8Srgb,
            color_transform,
            dmabuf: None,
            damage,
            destination,
            source_uv,
            transform,
            tint: [1.0; 4],
            retention: Some(FrameRetention::new(buffer.clone())),
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

#[cfg(test)]
mod color_transform_tests {
    use super::{font_atlas_needs_upload, texture_color_transform};
    use crate::core::color::{ColorDescription, RenderingIntent, SurfaceColorRenderState};
    use focaldesk_render::FrameTransferFunction;

    #[test]
    fn display_p3_surface_keeps_its_client_to_scene_matrix() {
        let color = SurfaceColorRenderState::for_description(
            ColorDescription::DISPLAY_P3_SRGB,
            RenderingIntent::Relative,
        );
        let texture = texture_color_transform(color);
        assert_eq!(texture.transfer, FrameTransferFunction::Srgb);
        assert_eq!(texture.client_to_scene, color.client_to_scene);
        assert_ne!(
            texture.client_to_scene,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
    }

    #[test]
    fn pq_surface_keeps_reference_white_for_absolute_decode() {
        let color = SurfaceColorRenderState::for_description(
            ColorDescription::bt2020_pq_hdr(600.0, 300.0),
            RenderingIntent::Perceptual,
        );
        let texture = texture_color_transform(color);
        assert_eq!(texture.transfer, FrameTransferFunction::St2084Pq);
        assert_eq!(
            texture.reference_white_nits,
            color.description.reference_white_nits
        );
    }

    #[test]
    fn lock_transition_reuploads_an_otherwise_clean_font_atlas() {
        assert!(font_atlas_needs_upload(false, true, true, false));
        assert!(!font_atlas_needs_upload(false, true, true, true));
        assert!(!font_atlas_needs_upload(false, true, false, true));
    }
}

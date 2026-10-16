//! Opt-in VUI shell surface hosted by the nested or raw Vulkan compositor.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

use anyhow::Result;
use focaldesk_render::{
    FramePixelFormat, FrameTransform, TextureColorTransform, TextureQuad, WgpuVulkanRenderer,
};
use focaldesk_ui::{
    types::{PanelKind, SettingKey, SystemCommand, UiAction},
    ui_builder::SIDEBAR_SETTINGS_ID,
};
#[cfg(feature = "drm-vulkan")]
use vui_ash::EmbeddedRenderer;
use vui_core::{
    Accordion, AccordionSection, BoxConstraints, Button, ButtonVariant, Color, Column,
    CrossAxisAlignment, Event, EventCtx, Flex, FlexChild, IconButton, IconData, IconName, Insets,
    Key, LayoutCtx, Modifier, Modifiers, OverlayHost, OverlayPlacement, Padding, PaintCtx, Panel,
    PathCommand, Point, PointerButton, Rect, SemanticNode, SemanticRole, SemanticsCtx, Size,
    SizedBox, Slider, Spacer, Stack, Surface, Switch, Theme, ThemeScope, UiRuntime, Widget,
    WidgetId,
};
use vui_text::ParleyTextSystem;
use vui_vello::TextureRenderer;

use super::wgpu_nested::VulkanCompositorScene;
use focaldesk_input::{InputEvent, KeyState, MouseButton, ScrollDelta};

const TEXTURE_CACHE_KEY: u64 = 0x5655_492d_5348_454c;
const MAX_SURFACE_LOGICAL_WIDTH: f64 = 560.0;
const MAX_SURFACE_LOGICAL_HEIGHT: f64 = 960.0;
const SHELL_FONT: &[u8] = include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf");

type ActionQueue = Rc<RefCell<Vec<UiAction>>>;

#[derive(Clone, Copy)]
pub(crate) struct QuickSettingsState {
    pub(crate) wifi: bool,
    pub(crate) bluetooth: bool,
    pub(crate) do_not_disturb: bool,
    pub(crate) volume: f64,
    pub(crate) night_light: bool,
}

pub(crate) struct VuiShell {
    runtime: UiRuntime,
    renderer: Option<TextureRenderer>,
    #[cfg(feature = "drm-vulkan")]
    ash_renderer: Option<EmbeddedRenderer>,
    started: Instant,
    scale_factor: f64,
    host_physical_width: u32,
    viewport: Size,
    right_inset_logical: f64,
    texture_size: Option<(u32, u32)>,
    rail_visible: bool,
    dock_visible: bool,
    overlay_open: Rc<Cell<bool>>,
    open_requested: Rc<Cell<bool>>,
    session_locked: bool,
    actions: ActionQueue,
}

impl VuiShell {
    pub(crate) fn enabled_from_env() -> bool {
        std::env::var("FOCALDESK_VUI").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    }

    fn rail_enabled_from_env() -> bool {
        env_flag("FOCALDESK_VUI_RAIL")
    }

    fn dock_enabled_from_env() -> bool {
        env_flag("FOCALDESK_VUI_DOCK")
    }

    pub(crate) fn new(
        width: u32,
        height: u32,
        scale_factor: f64,
        initial: QuickSettingsState,
    ) -> Self {
        Self::new_with_visibility(
            width,
            height,
            scale_factor,
            initial,
            Self::rail_enabled_from_env(),
            Self::dock_enabled_from_env(),
        )
    }

    fn new_with_visibility(
        width: u32,
        height: u32,
        scale_factor: f64,
        initial: QuickSettingsState,
        rail_visible: bool,
        dock_visible: bool,
    ) -> Self {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let settings = Rc::new(Cell::new(initial));
        let overlay_open = Rc::new(Cell::new(false));
        let open_requested = Rc::new(Cell::new(false));
        let trigger = QuickSettingsAnchor::new(
            open_requested.clone(),
            overlay_open.clone(),
            actions.clone(),
            settings.clone(),
            rail_visible,
        );
        let base = vui_shell_base(trigger, actions.clone(), rail_visible, dock_visible);
        let dismissed_overlay_open = overlay_open.clone();
        let root = OverlayHost::new(SizedBox::new(base).fill_width().fill_height())
            .on_dismiss(move || dismissed_overlay_open.set(false));
        let mut runtime = UiRuntime::new(root).with_text_system(
            ParleyTextSystem::with_embedded_fonts([SHELL_FONT])
                .expect("the embedded IBM Plex Sans shell font must be valid"),
        );
        runtime.set_transparent(true);
        runtime.set_theme(vui_shell_theme());
        let viewport = logical_size(width, height, scale_factor, dock_visible);
        runtime.set_viewport(viewport);
        // Lay out the invisible external anchor before the rail can request an
        // overlay. The closed shell never submits this frame to the compositor.
        let _ = runtime.render_frame();
        Self {
            runtime,
            renderer: None,
            #[cfg(feature = "drm-vulkan")]
            ash_renderer: None,
            started: Instant::now(),
            scale_factor: valid_scale(scale_factor),
            host_physical_width: width,
            viewport,
            right_inset_logical: 0.0,
            texture_size: None,
            rail_visible,
            dock_visible,
            overlay_open,
            open_requested,
            session_locked: false,
            actions,
        }
    }

    pub(crate) fn take_actions(&self) -> Vec<UiAction> {
        std::mem::take(&mut *self.actions.borrow_mut())
    }

    pub(crate) fn handle_text_input(&mut self, text: &str) -> bool {
        if self.session_locked
            || !self.overlay_open.get()
            || text.is_empty()
            || text.chars().any(char::is_control)
        {
            return false;
        }
        self.runtime
            .handle_event(&Event::TextInput(text.to_owned()))
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32, scale_factor: f64) {
        self.scale_factor = valid_scale(scale_factor);
        self.host_physical_width = width;
        let size = logical_size(width, height, self.scale_factor, self.dock_visible);
        self.viewport = size;
        self.runtime.set_viewport(size);
        self.runtime.handle_event(&Event::WindowResized(size));
        self.runtime
            .handle_event(&Event::WindowScaleFactorChanged(self.scale_factor));
    }

    pub(crate) fn tick(&mut self) -> bool {
        !self.session_locked
            && (self.rail_visible || self.dock_visible || self.overlay_open.get())
            && self.runtime.advance_time(self.started.elapsed())
    }

    pub(crate) fn set_right_inset(&mut self, inset: f64) {
        // A VUI-owned rail occupies the output edge itself. A temporary
        // one-pixel layer-shell companion may still publish an exclusive zone
        // for normal clients, but that reservation must not offset the VUI
        // surface that visually owns the edge.
        self.right_inset_logical = if self.rail_visible {
            0.0
        } else {
            inset.max(0.0)
        };
    }

    pub(crate) fn input_origin(&self) -> (f64, f64) {
        if self.dock_visible {
            return (0.0, 0.0);
        }
        let host_width = f64::from(self.host_physical_width) / self.scale_factor;
        (
            (host_width - self.right_inset_logical - self.viewport.width).max(0.0),
            0.0,
        )
    }

    pub(crate) fn toggle_quick_settings(&mut self) -> bool {
        if self.session_locked {
            return false;
        }
        if self.overlay_open.get() {
            self.runtime.handle_event(&Event::KeyDown(Key::Escape));
            self.runtime.handle_event(&Event::KeyUp(Key::Escape));
            self.overlay_open.set(false);
        } else {
            self.open_requested.set(true);
            self.overlay_open.set(true);
            self.runtime.handle_event(&Event::AppStateChanged);
            self.runtime.request_redraw();
        }
        true
    }

    pub(crate) fn quick_settings_open(&self) -> bool {
        self.overlay_open.get()
    }

    /// Hides compositor-owned controls while the lock screen owns the session.
    ///
    /// Dismissing the overlay here also prevents a focused quick-settings
    /// control from consuming the Enter key intended for password submission.
    pub(crate) fn set_session_locked(&mut self, locked: bool) {
        if self.session_locked == locked {
            return;
        }
        self.session_locked = locked;
        if locked && self.overlay_open.get() {
            self.runtime.handle_event(&Event::KeyDown(Key::Escape));
            self.runtime.handle_event(&Event::KeyUp(Key::Escape));
            self.overlay_open.set(false);
        }
    }

    pub(crate) fn damage_rect(&self) -> Option<[i32; 4]> {
        self.texture_size.map(|(width, height)| {
            let x = self.surface_origin_physical(width);
            [
                x,
                0,
                width.try_into().unwrap_or(i32::MAX),
                height.try_into().unwrap_or(i32::MAX),
            ]
        })
    }

    /// Uses the raw compositor's existing Vulkan device for direct rendering.
    ///
    /// # Safety
    ///
    /// The supplied handles must belong to one live Vulkan context, the queue
    /// must come from `queue_family`, and the shell must be dropped before the
    /// host renderer destroys that context.
    #[cfg(feature = "drm-vulkan")]
    pub(crate) unsafe fn use_ash_renderer(
        &mut self,
        instance: &ash::Instance,
        physical: ash::vk::PhysicalDevice,
        device: &ash::Device,
        queue: ash::vk::Queue,
        queue_family: u32,
    ) -> Result<()> {
        self.ash_renderer = Some(unsafe {
            EmbeddedRenderer::from_host_device(
                instance,
                physical,
                device,
                queue,
                queue_family,
                None,
            )?
        });
        Ok(())
    }

    #[cfg(feature = "drm-vulkan")]
    pub(crate) fn abandon_ash_renderer(&mut self) {
        if let Some(renderer) = self.ash_renderer.as_mut() {
            renderer.abandon_device();
        }
        self.ash_renderer = None;
    }

    pub(crate) fn handle_input(&mut self, event: &InputEvent) -> bool {
        self.handle_input_with_origin(event, (0.0, 0.0))
    }

    pub(crate) fn handle_input_with_origin(
        &mut self,
        event: &InputEvent,
        origin: (f64, f64),
    ) -> bool {
        if self.session_locked {
            return false;
        }
        let event = match event {
            InputEvent::PointerMoved { position, .. } => {
                Event::PointerMoved(Point::new(position.x - origin.0, position.y - origin.1))
            }
            InputEvent::PointerButton {
                button,
                state,
                position,
            } => {
                let Some(button) = pointer_button(*button) else {
                    return false;
                };
                let position = Point::new(position.x - origin.0, position.y - origin.1);
                match state {
                    KeyState::Pressed => Event::PointerDown {
                        position,
                        button,
                        click_count: 1,
                    },
                    KeyState::Released => Event::PointerUp { position, button },
                }
            }
            InputEvent::PointerScroll { delta, position } => {
                let (delta_x, delta_y) = scroll_delta(*delta);
                Event::Scroll {
                    position: Point::new(position.x - origin.0, position.y - origin.1),
                    delta_x,
                    delta_y,
                }
            }
            InputEvent::PointerLeft => Event::PointerLeft,
            InputEvent::Key {
                keycode,
                state,
                modifiers,
                ..
            } => {
                if !self.overlay_open.get() {
                    return false;
                }
                let mut vui_modifiers = Modifiers::default();
                vui_modifiers.set(Modifier::Shift, modifiers.shift);
                vui_modifiers.set(Modifier::Control, modifiers.ctrl);
                vui_modifiers.set(Modifier::Alt, modifiers.alt);
                vui_modifiers.set(Modifier::Meta, modifiers.super_key);
                self.runtime
                    .handle_event(&Event::ModifiersChanged(vui_modifiers));
                let key = key_from_evdev(*keycode);
                match state {
                    KeyState::Pressed => Event::KeyDown(key),
                    KeyState::Released => Event::KeyUp(key),
                }
            }
            InputEvent::PointerEntered
            | InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. } => return false,
        };
        self.runtime.handle_event(&event)
    }

    #[cfg(feature = "drm-vulkan")]
    pub(crate) fn composite_ash(
        &mut self,
        host: &mut focaldesk_render::AshDrmRenderer,
        scene: &mut VulkanCompositorScene,
    ) -> Result<()> {
        if self.session_locked
            || (!self.rail_visible && !self.dock_visible && !self.overlay_open.get())
        {
            return Ok(());
        }
        self.tick();
        if let Some(frame) = self.runtime.render_frame() {
            let rendered = self
                .ash_renderer
                .as_mut()
                .expect("raw Vulkan compositor configured an Ash VUI renderer")
                .render(&frame, self.scale_factor, vui_core::Color::rgba(0, 0, 0, 0))?;
            let image = rendered.image;
            let view = rendered.view;
            let width = rendered.width;
            let height = rendered.height;
            host.register_borrowed_texture(
                TEXTURE_CACHE_KEY,
                image,
                view,
                width,
                height,
                FramePixelFormat::Rgba8Srgb,
            )?;
            self.texture_size = Some((width, height));
            scene
                .damage
                .push(self.damage_rect().expect("texture size set above"));
        }
        self.insert_surface(scene, Vec::new(), Vec::new());
        Ok(())
    }

    pub(crate) fn composite(
        &mut self,
        host: &mut WgpuVulkanRenderer,
        scene: &mut VulkanCompositorScene,
    ) -> Result<()> {
        if self.session_locked
            || (!self.rail_visible && !self.dock_visible && !self.overlay_open.get())
        {
            return Ok(());
        }
        self.tick();
        if let Some(frame) = self.runtime.render_frame() {
            if self.renderer.is_none() {
                self.renderer = Some(TextureRenderer::new(host.device())?);
            }
            let rendered = self
                .renderer
                .as_mut()
                .expect("VUI renderer initialized above")
                .render(host.device(), host.queue(), &frame, self.scale_factor)?;
            host.register_shared_texture(
                TEXTURE_CACHE_KEY,
                rendered.texture,
                rendered.width,
                rendered.height,
                FramePixelFormat::Rgba8Srgb,
            );
            self.texture_size = Some((rendered.width, rendered.height));
            scene
                .damage
                .push(self.damage_rect().expect("texture size set above"));
        }

        self.insert_surface(scene, Vec::new(), Vec::new());
        Ok(())
    }

    fn insert_surface(
        &self,
        scene: &mut VulkanCompositorScene,
        pixels: Vec<u8>,
        damage: Vec<[u32; 4]>,
    ) {
        self.insert_surface_with_dmabuf(scene, pixels, damage, None);
    }

    fn insert_surface_with_dmabuf(
        &self,
        scene: &mut VulkanCompositorScene,
        pixels: Vec<u8>,
        damage: Vec<[u32; 4]>,
        dmabuf: Option<focaldesk_render::LinuxDmabuf>,
    ) {
        let Some((width, height)) = self.texture_size else {
            return;
        };
        let destination_x = self.surface_origin_physical(width);
        scene.surfaces.insert(
            scene.egui_before_surface,
            TextureQuad {
                cache_key: TEXTURE_CACHE_KEY,
                pixels,
                width,
                height,
                stride: width.saturating_mul(4),
                format: FramePixelFormat::Rgba8Srgb,
                color_transform: TextureColorTransform::default(),
                #[cfg(unix)]
                dmabuf,
                damage,
                destination: [
                    destination_x,
                    0,
                    width.try_into().unwrap_or(i32::MAX),
                    height.try_into().unwrap_or(i32::MAX),
                ],
                source_uv: [0.0, 0.0, 1.0, 1.0],
                transform: FrameTransform::Normal,
                tint: [1.0; 4],
                retention: None,
            },
        );
    }

    fn surface_origin_physical(&self, texture_width: u32) -> i32 {
        if self.dock_visible {
            return 0;
        }
        let inset = (self.right_inset_logical * self.scale_factor).round();
        let inset = if inset.is_finite() && inset > 0.0 {
            inset.min(f64::from(u32::MAX)) as u32
        } else {
            0
        };
        self.host_physical_width
            .saturating_sub(inset)
            .saturating_sub(texture_width)
            .try_into()
            .unwrap_or(i32::MAX)
    }
}

struct QuickSettingsAnchor {
    id: WidgetId,
    bounds: Rect,
    button: Option<IconButton>,
    open_requested: Rc<Cell<bool>>,
    overlay_open: Rc<Cell<bool>>,
    actions: ActionQueue,
    settings: Rc<Cell<QuickSettingsState>>,
}

impl QuickSettingsAnchor {
    fn new(
        open_requested: Rc<Cell<bool>>,
        overlay_open: Rc<Cell<bool>>,
        actions: ActionQueue,
        settings: Rc<Cell<QuickSettingsState>>,
        visible: bool,
    ) -> Self {
        let button = visible.then(|| {
            let requested = open_requested.clone();
            IconButton::new(IconName::Settings, "Quick Settings", move || {
                requested.set(true);
            })
            .tooltip("Quick Settings")
        });
        Self {
            id: WidgetId::next(),
            bounds: Rect::default(),
            button,
            open_requested,
            overlay_open,
            actions,
            settings,
        }
    }
}

impl Widget for QuickSettingsAnchor {
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, constraints: BoxConstraints) -> Size {
        let size = self.button.as_mut().map_or_else(
            || constraints.constrain(Size::new(1.0, 1.0)),
            |button| button.layout(ctx, constraints),
        );
        self.bounds.width = size.width;
        self.bounds.height = size.height;
        size
    }

    fn set_origin(&mut self, origin: Point) {
        self.bounds.x = origin.x;
        self.bounds.y = origin.y;
        if let Some(button) = self.button.as_mut() {
            button.set_origin(origin);
        }
    }

    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &Event) {
        if let Some(button) = self.button.as_mut() {
            button.event(ctx, event);
        }
        if self.open_requested.replace(false) {
            self.overlay_open.set(true);
            ctx.show_overlay(
                self.id,
                OverlayPlacement::Left,
                quick_settings_panel(self.actions.clone(), self.settings.clone()),
            );
            ctx.set_handled();
        }
    }

    fn widget_id(&self) -> Option<WidgetId> {
        Some(self.id)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) {
        if let Some(button) = self.button.as_ref() {
            button.paint(ctx);
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) {
        // OverlayHost resolves widget anchors through the semantic tree. This
        // node is visually empty, but its bounds keep the compositor-owned
        // panel attached to the rail edge instead of the surface origin.
        ctx.push(SemanticNode {
            id: self.id,
            role: SemanticRole::Group,
            bounds: self.bounds,
            label: "Quick Settings placement anchor".to_owned(),
            disabled: true,
            focused: false,
            checked: None,
            value: None,
            ime_cursor_area: None,
        });
        if let Some(button) = self.button.as_ref() {
            button.semantics(ctx);
        }
    }
}

fn vui_shell_base(
    trigger: QuickSettingsAnchor,
    actions: ActionQueue,
    rail_visible: bool,
    dock_visible: bool,
) -> impl Widget {
    let mut layers = Vec::new();
    if rail_visible {
        layers.push(Box::new(vui_system_rail(trigger, actions.clone())) as Box<dyn Widget>);
    } else {
        layers.push(Box::new(
            SizedBox::new(Padding::new(
                Insets {
                    left: 0.0,
                    top: 68.0,
                    right: 0.0,
                    bottom: 0.0,
                },
                Flex::row([
                    FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
                    FlexChild::fixed(trigger),
                ]),
            ))
            .fill_width()
            .fill_height(),
        ) as Box<dyn Widget>);
    }
    if dock_visible {
        layers.push(Box::new(vui_task_shelf(actions)) as Box<dyn Widget>);
    }
    Stack::new(layers)
        .horizontal_alignment(CrossAxisAlignment::Stretch)
        .vertical_alignment(CrossAxisAlignment::Stretch)
}

fn vui_system_rail(trigger: QuickSettingsAnchor, actions: ActionQueue) -> impl Widget {
    let primary = shell_group(
        Column::new([
            Box::new(rail_action_button(
                rail_icon(RailIcon::Assistant),
                "Open FocalDesk AI Console",
                actions.clone(),
                UiAction::LaunchApp("@ai-console".into()),
            )) as Box<dyn Widget>,
            Box::new(trigger) as Box<dyn Widget>,
        ])
        .gap(4.0),
    );

    let system = shell_group(
        Column::new([
            Box::new(rail_panel_button(
                RailIcon::Network,
                "Network",
                PanelKind::Network,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Audio,
                "Audio",
                PanelKind::Audio,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Bluetooth,
                "Bluetooth",
                PanelKind::Bluetooth,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Display,
                "Display settings",
                PanelKind::Display,
                actions.clone(),
            )) as Box<dyn Widget>,
        ])
        .gap(4.0),
    );

    let workspace_actions = actions.clone();
    let add_actions = actions.clone();
    let workspaces = shell_group(
        Column::new([
            Box::new(compact_button("1", move || {
                workspace_actions
                    .borrow_mut()
                    .push(UiAction::FocusWorkspace(0));
            })) as Box<dyn Widget>,
            Box::new(compact_button("2", {
                let actions = actions.clone();
                move || actions.borrow_mut().push(UiAction::FocusWorkspace(1))
            })) as Box<dyn Widget>,
            Box::new(compact_button("3", {
                let actions = actions.clone();
                move || actions.borrow_mut().push(UiAction::FocusWorkspace(2))
            })) as Box<dyn Widget>,
            Box::new(compact_button("4", {
                let actions = actions.clone();
                move || actions.borrow_mut().push(UiAction::FocusWorkspace(3))
            })) as Box<dyn Widget>,
            Box::new(compact_button("+", move || {
                add_actions
                    .borrow_mut()
                    .push(UiAction::CreateWorkspace("Workspace".into()));
            })) as Box<dyn Widget>,
        ])
        .gap(4.0),
    );

    let bottom = shell_group(
        Column::new([
            Box::new(rail_panel_button(
                RailIcon::Clock,
                "Calendar and clock",
                PanelKind::Calendar,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Notifications,
                "Notification center",
                PanelKind::NotificationHistory,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Updates,
                "System updates",
                PanelKind::Updates,
                actions.clone(),
            )) as Box<dyn Widget>,
            Box::new(rail_panel_button(
                RailIcon::Power,
                "Power menu",
                PanelKind::Power,
                actions,
            )) as Box<dyn Widget>,
        ])
        .gap(4.0),
    );

    let rail = Surface::new(
        Column::new([
            Box::new(primary) as Box<dyn Widget>,
            Box::new(system) as Box<dyn Widget>,
            Box::new(workspaces) as Box<dyn Widget>,
            Box::new(bottom) as Box<dyn Widget>,
        ])
        .gap(8.0),
    )
    .padding(Insets::all(7.0))
    .background(Color::rgba(20, 24, 32, 218))
    .border(Color::rgba(112, 128, 150, 194), 1.0)
    .radius(18.0)
    .elevation(2);

    SizedBox::new(Padding::new(
        Insets {
            left: 0.0,
            top: 8.0,
            right: 8.0,
            bottom: 8.0,
        },
        Flex::row([
            FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
            FlexChild::fixed(ThemeScope::new(vui_shell_theme(), rail)),
        ]),
    ))
    .fill_width()
    .fill_height()
}

fn vui_task_shelf(actions: ActionQueue) -> impl Widget {
    let pinned = Flex::row([
        FlexChild::fixed(rail_action_button(
            rail_icon(RailIcon::Browser),
            "Browser",
            actions.clone(),
            UiAction::LaunchApp("@browser".into()),
        )),
        FlexChild::fixed(rail_action_button(
            rail_icon(RailIcon::Terminal),
            "Terminal",
            actions.clone(),
            UiAction::LaunchApp("@terminal".into()),
        )),
        FlexChild::fixed(rail_action_button(
            rail_icon(RailIcon::Files),
            "Files",
            actions.clone(),
            UiAction::LaunchApp("@files".into()),
        )),
        FlexChild::fixed(rail_action_button(
            rail_icon(RailIcon::Mail),
            "Email",
            actions.clone(),
            UiAction::LaunchApp("@email".into()),
        )),
    ])
    .gap(4.0);
    let separator = SizedBox::new(
        Surface::new(Spacer::new(1.0, 30.0)).background(Color::rgba(112, 132, 156, 158)),
    )
    .width(1.0)
    .height(30.0);
    let utilities = Flex::row([
        FlexChild::fixed(rail_action_button(
            rail_icon(RailIcon::Applications),
            "Application launcher",
            actions.clone(),
            UiAction::LaunchApp("@launcher".into()),
        )),
        FlexChild::fixed(rail_action_button(
            IconName::Settings.data(),
            "Settings",
            actions,
            UiAction::LaunchApp("@settings".into()),
        )),
    ])
    .gap(4.0);
    let content = Flex::row([
        FlexChild::fixed(pinned),
        FlexChild::fixed(separator),
        FlexChild::fixed(utilities),
    ])
    .gap(8.0)
    .cross_alignment(CrossAxisAlignment::Center);
    let shelf = Surface::new(content)
        .padding(Insets {
            left: 10.0,
            top: 5.0,
            right: 10.0,
            bottom: 5.0,
        })
        .background(Color::rgba(20, 24, 32, 224))
        .border(Color::rgba(112, 128, 150, 194), 1.0)
        .radius(18.0)
        .elevation(2);
    let shelf_with_margin = Padding::new(
        Insets {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 22.0,
        },
        ThemeScope::new(vui_shell_theme(), shelf),
    );
    SizedBox::new(
        Stack::new([Box::new(shelf_with_margin) as Box<dyn Widget>])
            .horizontal_alignment(CrossAxisAlignment::Center)
            .vertical_alignment(CrossAxisAlignment::End),
    )
    .fill_width()
    .fill_height()
}

fn shell_group(child: impl Widget + 'static) -> Surface {
    Surface::new(child)
        .padding(Insets {
            left: 2.0,
            top: 2.0,
            right: 2.0,
            bottom: 2.0,
        })
        .background(Color::rgba(48, 57, 72, 34))
        .border(Color::rgba(112, 128, 150, 51), 1.0)
        .radius(15.0)
}

fn rail_panel_button(
    icon: RailIcon,
    label: &'static str,
    panel: PanelKind,
    actions: ActionQueue,
) -> IconButton {
    rail_action_button(rail_icon(icon), label, actions, UiAction::OpenPanel(panel))
}

fn rail_action_button(
    icon: IconData,
    label: &'static str,
    actions: ActionQueue,
    action: UiAction,
) -> IconButton {
    IconButton::data(icon, label, move || {
        actions.borrow_mut().push(action.clone());
    })
    .tooltip(label)
}

fn compact_button(label: &'static str, action: impl FnMut() + 'static) -> impl Widget {
    SizedBox::new(Button::new(label, action))
        .width(48.0)
        .height(40.0)
}

#[derive(Clone, Copy)]
enum RailIcon {
    Assistant,
    Network,
    Audio,
    Bluetooth,
    Display,
    Clock,
    Notifications,
    Updates,
    Power,
    Browser,
    Terminal,
    Files,
    Mail,
    Applications,
}

fn rail_icon(icon: RailIcon) -> IconData {
    use PathCommand::{CurveTo, LineTo, MoveTo};

    let p = Point::new;
    let commands = match icon {
        RailIcon::Assistant => return IconName::Command.data(),
        RailIcon::Network => vec![
            MoveTo(p(4.0, 9.0)),
            CurveTo(p(8.5, 5.0), p(15.5, 5.0), p(20.0, 9.0)),
            MoveTo(p(7.0, 13.0)),
            CurveTo(p(10.0, 10.5), p(14.0, 10.5), p(17.0, 13.0)),
            MoveTo(p(10.0, 17.0)),
            CurveTo(p(11.3, 16.0), p(12.7, 16.0), p(14.0, 17.0)),
            MoveTo(p(12.0, 20.0)),
            LineTo(p(12.0, 20.2)),
        ],
        RailIcon::Audio => vec![
            MoveTo(p(4.0, 10.0)),
            LineTo(p(8.0, 10.0)),
            LineTo(p(13.0, 6.0)),
            LineTo(p(13.0, 18.0)),
            LineTo(p(8.0, 14.0)),
            LineTo(p(4.0, 14.0)),
            LineTo(p(4.0, 10.0)),
            MoveTo(p(16.0, 9.0)),
            CurveTo(p(18.5, 10.5), p(18.5, 13.5), p(16.0, 15.0)),
            MoveTo(p(18.5, 6.5)),
            CurveTo(p(23.0, 9.5), p(23.0, 14.5), p(18.5, 17.5)),
        ],
        RailIcon::Bluetooth => vec![
            MoveTo(p(8.0, 7.0)),
            LineTo(p(17.0, 17.0)),
            LineTo(p(12.0, 21.0)),
            LineTo(p(12.0, 3.0)),
            LineTo(p(17.0, 7.0)),
            LineTo(p(8.0, 17.0)),
        ],
        RailIcon::Display => vec![
            MoveTo(p(4.0, 5.0)),
            LineTo(p(20.0, 5.0)),
            LineTo(p(20.0, 16.0)),
            LineTo(p(4.0, 16.0)),
            LineTo(p(4.0, 5.0)),
            MoveTo(p(9.0, 20.0)),
            LineTo(p(15.0, 20.0)),
            MoveTo(p(12.0, 16.0)),
            LineTo(p(12.0, 20.0)),
        ],
        RailIcon::Clock => vec![
            MoveTo(p(12.0, 3.0)),
            CurveTo(p(17.0, 3.0), p(21.0, 7.0), p(21.0, 12.0)),
            CurveTo(p(21.0, 17.0), p(17.0, 21.0), p(12.0, 21.0)),
            CurveTo(p(7.0, 21.0), p(3.0, 17.0), p(3.0, 12.0)),
            CurveTo(p(3.0, 7.0), p(7.0, 3.0), p(12.0, 3.0)),
            MoveTo(p(12.0, 7.0)),
            LineTo(p(12.0, 12.0)),
            LineTo(p(16.0, 14.0)),
        ],
        RailIcon::Notifications => vec![
            MoveTo(p(5.0, 17.0)),
            LineTo(p(7.0, 15.0)),
            LineTo(p(7.0, 10.0)),
            CurveTo(p(7.0, 3.5), p(17.0, 3.5), p(17.0, 10.0)),
            LineTo(p(17.0, 15.0)),
            LineTo(p(19.0, 17.0)),
            LineTo(p(5.0, 17.0)),
            MoveTo(p(10.0, 20.0)),
            CurveTo(p(11.0, 21.0), p(13.0, 21.0), p(14.0, 20.0)),
        ],
        RailIcon::Updates => vec![
            MoveTo(p(5.0, 9.0)),
            CurveTo(p(7.0, 4.0), p(14.0, 2.5), p(18.0, 6.0)),
            LineTo(p(20.0, 8.0)),
            MoveTo(p(20.0, 4.0)),
            LineTo(p(20.0, 8.0)),
            LineTo(p(16.0, 8.0)),
            MoveTo(p(19.0, 15.0)),
            CurveTo(p(17.0, 20.0), p(10.0, 21.5), p(6.0, 18.0)),
            LineTo(p(4.0, 16.0)),
            MoveTo(p(4.0, 20.0)),
            LineTo(p(4.0, 16.0)),
            LineTo(p(8.0, 16.0)),
        ],
        RailIcon::Power => vec![
            MoveTo(p(12.0, 3.0)),
            LineTo(p(12.0, 12.0)),
            MoveTo(p(7.5, 6.0)),
            CurveTo(p(1.5, 10.5), p(4.5, 20.5), p(12.0, 21.0)),
            CurveTo(p(19.5, 20.5), p(22.5, 10.5), p(16.5, 6.0)),
        ],
        RailIcon::Browser => vec![
            MoveTo(p(12.0, 3.0)),
            CurveTo(p(17.0, 3.0), p(21.0, 7.0), p(21.0, 12.0)),
            CurveTo(p(21.0, 17.0), p(17.0, 21.0), p(12.0, 21.0)),
            CurveTo(p(7.0, 21.0), p(3.0, 17.0), p(3.0, 12.0)),
            CurveTo(p(3.0, 7.0), p(7.0, 3.0), p(12.0, 3.0)),
            MoveTo(p(3.5, 12.0)),
            LineTo(p(20.5, 12.0)),
            MoveTo(p(12.0, 3.5)),
            CurveTo(p(8.5, 7.0), p(8.5, 17.0), p(12.0, 20.5)),
            MoveTo(p(12.0, 3.5)),
            CurveTo(p(15.5, 7.0), p(15.5, 17.0), p(12.0, 20.5)),
        ],
        RailIcon::Terminal => vec![
            MoveTo(p(4.0, 5.0)),
            LineTo(p(20.0, 5.0)),
            LineTo(p(20.0, 19.0)),
            LineTo(p(4.0, 19.0)),
            LineTo(p(4.0, 5.0)),
            MoveTo(p(7.0, 9.0)),
            LineTo(p(10.0, 12.0)),
            LineTo(p(7.0, 15.0)),
            MoveTo(p(12.0, 15.0)),
            LineTo(p(17.0, 15.0)),
        ],
        RailIcon::Files => vec![
            MoveTo(p(3.0, 7.0)),
            LineTo(p(10.0, 7.0)),
            LineTo(p(12.0, 9.0)),
            LineTo(p(21.0, 9.0)),
            LineTo(p(21.0, 19.0)),
            LineTo(p(3.0, 19.0)),
            LineTo(p(3.0, 7.0)),
        ],
        RailIcon::Mail => vec![
            MoveTo(p(3.0, 6.0)),
            LineTo(p(21.0, 6.0)),
            LineTo(p(21.0, 18.0)),
            LineTo(p(3.0, 18.0)),
            LineTo(p(3.0, 6.0)),
            MoveTo(p(3.5, 7.0)),
            LineTo(p(12.0, 13.0)),
            LineTo(p(20.5, 7.0)),
        ],
        RailIcon::Applications => vec![
            MoveTo(p(5.0, 5.0)),
            LineTo(p(8.0, 5.0)),
            LineTo(p(8.0, 8.0)),
            LineTo(p(5.0, 8.0)),
            LineTo(p(5.0, 5.0)),
            MoveTo(p(10.5, 5.0)),
            LineTo(p(13.5, 5.0)),
            LineTo(p(13.5, 8.0)),
            LineTo(p(10.5, 8.0)),
            LineTo(p(10.5, 5.0)),
            MoveTo(p(16.0, 5.0)),
            LineTo(p(19.0, 5.0)),
            LineTo(p(19.0, 8.0)),
            LineTo(p(16.0, 8.0)),
            LineTo(p(16.0, 5.0)),
            MoveTo(p(5.0, 10.5)),
            LineTo(p(8.0, 10.5)),
            LineTo(p(8.0, 13.5)),
            LineTo(p(5.0, 13.5)),
            LineTo(p(5.0, 10.5)),
            MoveTo(p(10.5, 10.5)),
            LineTo(p(13.5, 10.5)),
            LineTo(p(13.5, 13.5)),
            LineTo(p(10.5, 13.5)),
            LineTo(p(10.5, 10.5)),
            MoveTo(p(16.0, 10.5)),
            LineTo(p(19.0, 10.5)),
            LineTo(p(19.0, 13.5)),
            LineTo(p(16.0, 13.5)),
            LineTo(p(16.0, 10.5)),
            MoveTo(p(5.0, 16.0)),
            LineTo(p(8.0, 16.0)),
            LineTo(p(8.0, 19.0)),
            LineTo(p(5.0, 19.0)),
            LineTo(p(5.0, 16.0)),
            MoveTo(p(10.5, 16.0)),
            LineTo(p(13.5, 16.0)),
            LineTo(p(13.5, 19.0)),
            LineTo(p(10.5, 19.0)),
            LineTo(p(10.5, 16.0)),
            MoveTo(p(16.0, 16.0)),
            LineTo(p(19.0, 16.0)),
            LineTo(p(19.0, 19.0)),
            LineTo(p(16.0, 19.0)),
            LineTo(p(16.0, 16.0)),
        ],
    };
    IconData::new(Size::new(24.0, 24.0), commands)
}

fn vui_shell_theme() -> Theme {
    let mut theme = Theme::dark();
    theme.colors.window = Color::rgba(0, 0, 0, 0);
    theme.colors.surface = Color::rgba(38, 45, 58, 220);
    theme.colors.surface_raised = Color::rgba(46, 55, 70, 235);
    theme.colors.text = Color::rgb(223, 232, 242);
    theme.colors.text_muted = Color::rgb(151, 166, 184);
    theme.colors.border = Color::rgba(111, 132, 156, 128);
    theme.colors.focus = Color::rgb(102, 190, 244);
    theme.colors.primary = Color::rgb(49, 111, 165);
    theme.colors.primary_hover = Color::rgb(60, 130, 190);
    theme.colors.primary_pressed = Color::rgb(42, 96, 144);
    theme.colors.secondary = Color::rgba(93, 110, 133, 58);
    theme.colors.secondary_hover = Color::rgba(184, 204, 225, 42);
    theme.components.icon_button.height = 48.0;
    theme.components.icon_button.min_width = 48.0;
    theme.components.icon_button.radius = 14.0;
    theme.components.icon_button.border_width = 1.0;
    theme.components.icon_button.normal.background = Color::rgba(74, 88, 110, 26);
    theme.components.icon_button.normal.border = Color::rgba(112, 132, 156, 36);
    theme.components.icon_button.hovered.background = Color::rgba(223, 232, 242, 26);
    theme.components.icon_button.hovered.border = Color::rgba(112, 132, 156, 140);
    theme.components.icon_button.pressed.background = Color::rgba(70, 145, 205, 54);
    theme.components.icon_button.pressed.border = Color::rgba(102, 190, 244, 120);
    theme.components.secondary_button.height = 40.0;
    theme.components.secondary_button.min_width = 40.0;
    theme.components.secondary_button.radius = 12.0;
    theme.elevation.shadow = Color::rgba(0, 0, 0, 150);
    theme.window = theme.colors.window;
    theme.text = theme.colors.text;
    theme.focus = theme.colors.focus;
    theme
}

fn quick_settings_panel(
    actions: ActionQueue,
    settings: Rc<Cell<QuickSettingsState>>,
) -> impl Widget {
    quick_settings_view(actions, settings)
}

fn quick_settings_view(
    actions: ActionQueue,
    settings: Rc<Cell<QuickSettingsState>>,
) -> impl Widget {
    let network_actions = actions.clone();
    let network_settings = settings.clone();
    let wifi = Switch::new("Wi-Fi", settings.get().wifi, move |enabled| {
        let mut current = network_settings.get();
        current.wifi = enabled;
        network_settings.set(current);
        network_actions
            .borrow_mut()
            .push(UiAction::SetSetting(SettingKey::Wifi, enabled));
    });
    let bluetooth_actions = actions.clone();
    let bluetooth_settings = settings.clone();
    let bluetooth = Switch::new("Bluetooth", settings.get().bluetooth, move |enabled| {
        let mut current = bluetooth_settings.get();
        current.bluetooth = enabled;
        bluetooth_settings.set(current);
        bluetooth_actions
            .borrow_mut()
            .push(UiAction::SetSetting(SettingKey::Bluetooth, enabled));
    });
    let network = Column::new([
        Box::new(wifi) as Box<dyn Widget>,
        Box::new(bluetooth) as Box<dyn Widget>,
    ])
    .gap(10.0);

    let volume_actions = actions.clone();
    let volume_settings = settings.clone();
    let volume = Slider::new(
        "Volume",
        settings.get().volume * 100.0,
        0.0..=100.0,
        move |value| {
            let mut current = volume_settings.get();
            current.volume = value / 100.0;
            volume_settings.set(current);
            volume_actions
                .borrow_mut()
                .push(UiAction::SetVolume(volume_f32(current.volume)));
        },
    )
    .step(1.0);
    let audio = Column::new([Box::new(volume) as Box<dyn Widget>]).gap(10.0);

    let dnd_actions = actions.clone();
    let dnd_settings = settings.clone();
    let dnd = Switch::new(
        "Do Not Disturb",
        settings.get().do_not_disturb,
        move |enabled| {
            let mut current = dnd_settings.get();
            current.do_not_disturb = enabled;
            dnd_settings.set(current);
            dnd_actions
                .borrow_mut()
                .push(UiAction::SetSetting(SettingKey::DoNotDisturb, enabled));
        },
    );
    let focus = Column::new([Box::new(dnd) as Box<dyn Widget>]).gap(10.0);

    let comfort_actions = actions.clone();
    let comfort_settings = settings.clone();
    let night_light_label = if settings.get().night_light {
        "Night Light — active"
    } else {
        "Night Light — off"
    };
    let night_light = Switch::new(
        night_light_label,
        settings.get().night_light,
        move |enabled| {
            let mut current = comfort_settings.get();
            current.night_light = enabled;
            comfort_settings.set(current);
            comfort_actions
                .borrow_mut()
                .push(UiAction::SetNightLight(enabled));
        },
    );
    let comfort = Column::new([Box::new(night_light) as Box<dyn Widget>]).gap(10.0);

    let accordion = Accordion::new([
        AccordionSection::new("Network", network).expanded(true),
        AccordionSection::new("Audio", audio),
        AccordionSection::new("Focus", focus),
        AccordionSection::new("Display", comfort),
    ]);
    let lock_actions = actions.clone();
    let settings_actions = actions.clone();
    let actions_row = Flex::row([
        FlexChild::fixed(Button::new("Lock", move || {
            lock_actions
                .borrow_mut()
                .push(UiAction::SystemCommand(SystemCommand::Lock));
        })),
        FlexChild::fixed(
            Button::new("Settings", move || {
                settings_actions
                    .borrow_mut()
                    .push(UiAction::Custom(SIDEBAR_SETTINGS_ID));
            })
            .variant(ButtonVariant::Secondary),
        ),
    ])
    .gap(8.0);
    let body = Column::new([
        Box::new(accordion) as Box<dyn Widget>,
        Box::new(actions_row) as Box<dyn Widget>,
    ])
    .gap(12.0);
    SizedBox::new(Panel::dismissible("Quick Settings", body)).width(400.0)
}

#[allow(clippy::cast_possible_truncation)]
fn volume_f32(value: f64) -> f32 {
    value.clamp(0.0, 1.0) as f32
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn valid_scale(scale_factor: f64) -> f64 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn logical_size(width: u32, height: u32, scale_factor: f64, full_output: bool) -> Size {
    let scale_factor = valid_scale(scale_factor);
    Size::new(
        if full_output {
            f64::from(width) / scale_factor
        } else {
            (f64::from(width) / scale_factor).min(MAX_SURFACE_LOGICAL_WIDTH)
        },
        if full_output {
            f64::from(height) / scale_factor
        } else {
            (f64::from(height) / scale_factor).min(MAX_SURFACE_LOGICAL_HEIGHT)
        },
    )
}

fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Right => Some(PointerButton::Secondary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}

fn scroll_delta(delta: ScrollDelta) -> (f64, f64) {
    match delta {
        ScrollDelta::Line { x, y } => (f64::from(x) * 40.0, f64::from(y) * 40.0),
        ScrollDelta::Pixel { x, y } | ScrollDelta::Axis { x, y, .. } => (x, y),
    }
}

fn key_from_evdev(keycode: u32) -> Key {
    match keycode {
        1 => Key::Escape,
        14 => Key::Backspace,
        15 => Key::Tab,
        28 => Key::Enter,
        57 => Key::Space,
        102 => Key::Home,
        103 => Key::ArrowUp,
        104 => Key::PageUp,
        105 => Key::ArrowLeft,
        106 => Key::ArrowRight,
        107 => Key::End,
        108 => Key::ArrowDown,
        109 => Key::PageDown,
        111 => Key::Delete,
        _ => Key::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositor_shell_starts_closed_and_does_not_steal_space() {
        let mut shell = VuiShell::new_with_visibility(
            1920,
            1080,
            1.0,
            QuickSettingsState {
                wifi: true,
                bluetooth: false,
                do_not_disturb: false,
                volume: 0.7,
                night_light: false,
            },
            false,
            false,
        );
        assert!(!shell.overlay_open.get());
        assert!(!shell.handle_input(&InputEvent::Key {
            keycode: 57,
            state: KeyState::Pressed,
            repeat: false,
            modifiers: focaldesk_input::Modifiers::default(),
        }));

        shell.set_right_inset(80.0);
        assert_eq!(shell.input_origin(), (1280.0, 0.0));
        assert!(shell.toggle_quick_settings());
        assert!(shell.overlay_open.get());
        let opened = shell.runtime.render_frame().expect("opened shell frame");
        assert!(opened.semantics.iter().any(|node| node.label == "Wi-Fi"));
        let panel = opened
            .semantics
            .iter()
            .find(|node| node.label == "Quick Settings")
            .expect("quick settings panel");
        assert!(panel.bounds.x > 140.0, "panel should hug the right edge");
        assert_eq!(panel.bounds.y, 68.0);
        assert!(!opened.semantics.iter().any(|node| {
            node.role == vui_core::SemanticRole::Button && node.label == "Quick Settings"
        }));
        assert!(
            opened
                .paint
                .operations()
                .iter()
                .any(|operation| matches!(operation, vui_core::PaintOp::GlyphRun { .. }))
        );
        assert!(
            !opened
                .paint
                .operations()
                .iter()
                .any(|operation| matches!(operation, vui_core::PaintOp::Text { .. }))
        );

        assert!(shell.handle_input(&InputEvent::PointerButton {
            button: MouseButton::Left,
            state: KeyState::Pressed,
            position: (10.0, 900.0).into(),
        }));
        assert!(!shell.overlay_open.get());
        assert!(!shell.handle_input(&InputEvent::Key {
            keycode: 57,
            state: KeyState::Pressed,
            repeat: false,
            modifiers: focaldesk_input::Modifiers::default(),
        }));
    }

    #[test]
    fn locked_session_dismisses_quick_settings_and_releases_enter() {
        let mut shell = VuiShell::new_with_visibility(
            1920,
            1080,
            1.0,
            QuickSettingsState {
                wifi: true,
                bluetooth: false,
                do_not_disturb: false,
                volume: 0.7,
                night_light: false,
            },
            false,
            false,
        );
        assert!(shell.toggle_quick_settings());
        assert!(shell.overlay_open.get());

        shell.set_session_locked(true);

        assert!(!shell.overlay_open.get());
        assert!(!shell.handle_input(&InputEvent::Key {
            keycode: 28,
            state: KeyState::Pressed,
            repeat: false,
            modifiers: focaldesk_input::Modifiers::default(),
        }));
        let locked = shell.runtime.render_frame().expect("lock transition frame");
        assert!(!locked.semantics.iter().any(|node| node.label == "Wi-Fi"));
    }

    #[test]
    fn quick_settings_exposes_and_queues_typed_controls() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let settings = Rc::new(Cell::new(QuickSettingsState {
            wifi: true,
            bluetooth: false,
            do_not_disturb: false,
            volume: 0.7,
            night_light: false,
        }));
        let mut runtime = UiRuntime::new(quick_settings_panel(actions.clone(), settings));
        runtime.set_viewport(Size::new(500.0, 800.0));
        let frame = runtime.render_frame().expect("initial frame");
        let wifi = frame
            .semantics
            .iter()
            .find(|node| node.role == vui_core::SemanticRole::Switch && node.label == "Wi-Fi")
            .expect("Wi-Fi control")
            .bounds;
        for label in ["Audio", "Focus", "Lock", "Settings"] {
            assert!(frame.semantics.iter().any(|node| node.label == label));
        }
        assert!(
            !frame
                .semantics
                .iter()
                .any(|node| node.label == "VUI compositor surface")
        );

        let position = Point::new(wifi.x + wifi.width / 2.0, wifi.y + wifi.height / 2.0);
        assert!(runtime.handle_event(&Event::PointerDown {
            position,
            button: PointerButton::Primary,
            click_count: 1,
        }));
        assert!(runtime.handle_event(&Event::PointerUp {
            position,
            button: PointerButton::Primary,
        }));
        assert!(matches!(
            actions.borrow().as_slice(),
            [UiAction::SetSetting(SettingKey::Wifi, false)]
        ));
    }

    #[test]
    fn vui_rail_exposes_shell_actions_and_anchors_quick_settings() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let settings = Rc::new(Cell::new(QuickSettingsState {
            wifi: true,
            bluetooth: false,
            do_not_disturb: false,
            volume: 0.7,
            night_light: false,
        }));
        let requested = Rc::new(Cell::new(false));
        let overlay_open = Rc::new(Cell::new(false));
        let trigger = QuickSettingsAnchor::new(
            requested,
            overlay_open.clone(),
            actions.clone(),
            settings,
            true,
        );
        let mut runtime = UiRuntime::new(OverlayHost::new(
            SizedBox::new(vui_system_rail(trigger, actions))
                .fill_width()
                .fill_height(),
        ));
        runtime.set_transparent(true);
        runtime.set_theme(vui_shell_theme());
        runtime.set_viewport(Size::new(560.0, 960.0));
        let frame = runtime.render_frame().expect("rail frame");
        for label in [
            "Open FocalDesk AI Console",
            "Quick Settings",
            "Network",
            "Audio",
            "Bluetooth",
            "Display settings",
            "Calendar and clock",
            "Notification center",
            "System updates",
            "Power menu",
        ] {
            assert!(frame.semantics.iter().any(|node| node.label == label));
        }

        let quick_settings = frame
            .semantics
            .iter()
            .find(|node| node.role == SemanticRole::Button && node.label == "Quick Settings")
            .expect("quick settings rail button")
            .bounds;
        let position = Point::new(
            quick_settings.x + quick_settings.width / 2.0,
            quick_settings.y + quick_settings.height / 2.0,
        );
        assert!(runtime.handle_event(&Event::PointerDown {
            position,
            button: PointerButton::Primary,
            click_count: 1,
        }));
        assert!(runtime.handle_event(&Event::PointerUp {
            position,
            button: PointerButton::Primary,
        }));
        assert!(overlay_open.get());
        let opened = runtime.render_frame().expect("quick settings overlay");
        assert!(
            opened.semantics.iter().any(|node| node.label == "Wi-Fi"),
            "overlay semantics: {:?}",
            opened
                .semantics
                .iter()
                .map(|node| node.label.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn vui_task_shelf_is_bottom_centered_and_launches_pinned_apps() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = UiRuntime::new(vui_task_shelf(actions.clone()));
        runtime.set_transparent(true);
        runtime.set_theme(vui_shell_theme());
        runtime.set_viewport(Size::new(1920.0, 1080.0));
        let frame = runtime.render_frame().expect("task shelf frame");
        for label in [
            "Browser",
            "Terminal",
            "Files",
            "Email",
            "Application launcher",
            "Settings",
        ] {
            assert!(frame.semantics.iter().any(|node| node.label == label));
        }
        let browser = frame
            .semantics
            .iter()
            .find(|node| node.role == SemanticRole::Button && node.label == "Browser")
            .expect("browser shelf button")
            .bounds;
        assert!(browser.x > 700.0, "shelf should be centered");
        assert!(browser.y > 990.0, "shelf should sit near the bottom edge");
        let position = Point::new(
            browser.x + browser.width / 2.0,
            browser.y + browser.height / 2.0,
        );
        assert!(runtime.handle_event(&Event::PointerDown {
            position,
            button: PointerButton::Primary,
            click_count: 1,
        }));
        assert!(runtime.handle_event(&Event::PointerUp {
            position,
            button: PointerButton::Primary,
        }));
        assert!(matches!(
            actions.borrow().as_slice(),
            [UiAction::LaunchApp(app)] if app == "@browser"
        ));
    }
}

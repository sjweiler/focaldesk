#![allow(unused_imports)]

use focaldesk_types::types::{OutputId, WindowId, WorkspaceId};
use smithay::desktop::{PopupManager, Space};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::core::desktop::DesktopState;
use crate::core::shell::xwayland::{XwaylandSurfaceRole, XwaylandWindowMeta};
use crate::core::shell::WaylandWindowMeta;
use smithay::desktop::WindowSurface;
use smithay::input::dnd::{DndFocus, OfferData as DndOfferDataTrait, Source};
use smithay::input::pointer::AxisFrame;
use smithay::input::pointer::ButtonEvent;
use smithay::input::pointer::MotionEvent;
use smithay::input::pointer::PointerTarget;
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Point;
use smithay::utils::Serial;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::ToplevelSurface;
#[cfg(feature = "xwayland")]
use smithay::xwayland::X11Surface;
use smithay::{
    desktop::Window,
    utils::{IsAlive, Logical, Rectangle},
};
use std::borrow::Cow;
use tracing_subscriber::fmt::time;
use wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use wayland_server::DisplayHandle;

impl PartialEq for ManagedWindow {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl IsAlive for ManagedWindow {
    fn alive(&self) -> bool {
        if let Some(surface) = self.wl_surface() {
            return surface.alive();
        }

        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            return x11.alive();
        }

        false
    }
}

impl DndFocus<DesktopState> for ManagedWindow {
    type OfferData<S>
        = <WlSurface as DndFocus<DesktopState>>::OfferData<S>
    where
        S: Source;

    fn enter<S: Source>(
        &self,
        data: &mut DesktopState,
        dh: &DisplayHandle,
        source: std::sync::Arc<S>,
        seat: &Seat<DesktopState>,
        location: Point<f64, Logical>,
        serial: &Serial,
    ) -> Option<Self::OfferData<S>> {
        if let Some(surface) = self.window.wl_surface() {
            return DndFocus::enter(&*surface, data, dh, source, seat, location, serial);
        }

        None
    }

    fn motion<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
        location: Point<f64, Logical>,
        time: u32,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            DndFocus::motion(&*surface, data, offer, seat, location, time);
        }
    }

    fn leave<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            DndFocus::leave(&*surface, data, offer, seat);
        }
    }

    fn drop<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            DndFocus::drop(&*surface, data, offer, seat);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowProtocol {
    Wayland,
    Xwayland,
}

#[derive(Debug, Clone)]
pub enum ManagedWindowKind {
    Wayland(WaylandWindowMeta),
    Xwayland(XwaylandWindowMeta),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManagedSurface {
    Wayland(Window),
    #[cfg(feature = "xwayland")]
    X11(X11Surface),
}

#[derive(Debug, Clone)]
pub struct ManagedWindow {
    pub id: WindowId,
    pub window: Window,
    pub kind: ManagedWindowKind,

    // compositor / shell state
    pub mapped: bool,
    pub floating: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub activated: bool,
    pub urgent: bool,
    pub pending_move: bool,
    pub pending_resize: Option<ResizeEdge>,
    pub workspace: WorkspaceId,
    pub output: Option<OutputId>,

    // layout state
    pub tile_rect: Option<Rectangle<i32, Logical>>,
    pub float_rect: Option<Rectangle<i32, Logical>>,
    pub restore_rect: Option<Rectangle<i32, Logical>>,
}

impl ManagedWindow {
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.window.wl_surface()
    }
    pub fn matches_toplevel(&self, surface: &ToplevelSurface) -> bool {
        self.window
            .wl_surface()
            .map(|wl| wl == std::borrow::Cow::Borrowed(surface.wl_surface()))
            .unwrap_or(false)
    }
    pub fn new_wayland(
        id: WindowId,
        window: Window,
        meta: WaylandWindowMeta,
        workspace: WorkspaceId,
    ) -> Self {
        Self {
            id,
            window,
            kind: ManagedWindowKind::Wayland(meta),
            mapped: false,
            floating: false,
            maximized: false,
            fullscreen: false,
            minimized: false,
            activated: false,
            urgent: false,
            workspace,
            output: None,
            tile_rect: None,
            float_rect: None,
            restore_rect: None,
            pending_move: false,
            pending_resize: None,
        }
    }

    pub fn new_xwayland(
        id: WindowId,
        window: Window,
        meta: XwaylandWindowMeta,
        workspace: WorkspaceId,
    ) -> Self {
        Self {
            id,
            window,
            kind: ManagedWindowKind::Xwayland(meta),
            mapped: true,
            floating: true, // safer default for early X11 support
            maximized: false,
            fullscreen: false,
            minimized: false,
            activated: false,
            urgent: false,
            workspace,
            output: None,
            tile_rect: None,
            float_rect: None,
            restore_rect: None,
            pending_move: false,
            pending_resize: None,
        }
    }

    pub fn maximize() {}

    pub fn protocol(&self) -> WindowProtocol {
        match self.kind {
            ManagedWindowKind::Wayland(_) => WindowProtocol::Wayland,
            ManagedWindowKind::Xwayland(_) => WindowProtocol::Xwayland,
        }
    }

    pub fn title(&self) -> String {
        match &self.kind {
            ManagedWindowKind::Wayland(meta) => {
                meta.title.clone().unwrap_or_else(|| "Untitled".into())
            }
            ManagedWindowKind::Xwayland(meta) => meta
                .title
                .clone()
                .unwrap_or_else(|| meta.class.clone().unwrap_or_else(|| "X11 App".into())),
        }
    }

    pub fn app_id(&self) -> Option<&str> {
        match &self.kind {
            ManagedWindowKind::Wayland(meta) => meta.app_id.as_deref(),
            ManagedWindowKind::Xwayland(_) => None,
        }
    }

    pub fn class(&self) -> Option<&str> {
        match &self.kind {
            ManagedWindowKind::Wayland(_) => None,
            ManagedWindowKind::Xwayland(meta) => meta.class.as_deref(),
        }
    }

    pub fn display_name(&self) -> String {
        if let Some(app_id) = self.app_id() {
            return app_id.to_string();
        }
        if let Some(class) = self.class() {
            return class.to_string();
        }
        self.title()
    }

    pub fn geometry(&self) -> Rectangle<i32, Logical> {
        self.window.geometry()
    }

    pub fn bbox(&self) -> Rectangle<i32, Logical> {
        self.window.bbox()
    }

    pub fn is_dialog_like(&self) -> bool {
        match &self.kind {
            ManagedWindowKind::Wayland(meta) => meta.is_dialog,
            ManagedWindowKind::Xwayland(meta) => {
                matches!(
                    meta.role,
                    XwaylandSurfaceRole::Dialog | XwaylandSurfaceRole::Transient
                )
            }
        }
    }

    pub fn is_override_redirect(&self) -> bool {
        match &self.kind {
            ManagedWindowKind::Wayland(_) => false,
            ManagedWindowKind::Xwayland(meta) => meta.override_redirect,
        }
    }

    pub fn set_activated(&mut self, active: bool) {
        self.activated = active;

        // protocol-specific activation hooks can be added here later
        // once your Smithay integration code is in place
    }

    pub fn request_close(&self) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.send_close();
            }
            WindowSurface::X11(surface) => {
                if let Err(err) = surface.close() {
                    focaldesk_logging::flog(format!(
                        "failed to send XWayland close request: {err:?}"
                    ));
                }
            }
        }
    }

    pub fn set_fullscreen(&mut self, value: bool) {
        self.fullscreen = value;
    }

    pub fn set_maximized(&mut self, value: bool) {
        self.maximized = value;
    }

    pub fn set_floating(&mut self, value: bool) {
        self.floating = value;
    }

    pub fn set_workspace(&mut self, workspace: WorkspaceId) {
        self.workspace = workspace;
    }

    pub fn set_output(&mut self, output: Option<OutputId>) {
        self.output = output;
    }

    pub fn current_rect(&self) -> Rectangle<i32, Logical> {
        if self.floating {
            self.float_rect.unwrap_or_else(|| self.geometry())
        } else {
            self.tile_rect.unwrap_or_else(|| self.geometry())
        }
    }
}

impl WaylandFocus for ManagedWindow {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.wl_surface()
    }
}

use smithay::input::pointer::*;

impl PointerTarget<DesktopState> for ManagedWindow {
    fn enter(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &MotionEvent) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::enter(x11, seat, data, event);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::enter(&*surface, seat, data, event);
        }
    }

    fn motion(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &MotionEvent) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::motion(x11, seat, data, event);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::motion(&*surface, seat, data, event);
        }
    }

    fn relative_motion(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &RelativeMotionEvent,
    ) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::relative_motion(x11, seat, data, event);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::relative_motion(&*surface, seat, data, event);
        }
    }

    fn button(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &ButtonEvent) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::button(x11, seat, data, event);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::button(&*surface, seat, data, event);
        }
    }

    fn axis(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, frame: AxisFrame) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::axis(x11, seat, data, frame);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::axis(&*surface, seat, data, frame);
        }
    }

    fn frame(&self, seat: &Seat<DesktopState>, data: &mut DesktopState) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::frame(x11, seat, data);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::frame(&*surface, seat, data);
        }
    }

    fn leave(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        serial: smithay::utils::Serial,
        time: u32,
    ) {
        #[cfg(feature = "xwayland")]
        if let Some(x11) = self.window.x11_surface() {
            PointerTarget::leave(x11, seat, data, serial, time);
            return;
        }

        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::leave(&*surface, seat, data, serial, time);
        }
    }

    fn gesture_swipe_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeBeginEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_swipe_begin(&*surface, seat, data, event);
        }
    }

    fn gesture_swipe_update(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeUpdateEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_swipe_update(&*surface, seat, data, event);
        }
    }

    fn gesture_swipe_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeEndEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_swipe_end(&*surface, seat, data, event);
        }
    }

    fn gesture_pinch_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchBeginEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_pinch_begin(&*surface, seat, data, event);
        }
    }

    fn gesture_pinch_update(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchUpdateEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_pinch_update(&*surface, seat, data, event);
        }
    }

    fn gesture_pinch_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchEndEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_pinch_end(&*surface, seat, data, event);
        }
    }

    fn gesture_hold_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureHoldBeginEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_hold_begin(&*surface, seat, data, event);
        }
    }

    fn gesture_hold_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureHoldEndEvent,
    ) {
        if let Some(surface) = self.window.wl_surface() {
            PointerTarget::gesture_hold_end(&*surface, seat, data, event);
        }
    }
}

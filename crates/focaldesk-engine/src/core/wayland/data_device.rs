#![allow(unused_imports)]

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::core::desktop::{DesktopState, DND_CURSOR_ENDED, DND_CURSOR_INVALID, DND_CURSOR_VALID};

use smithay::input::dnd::{
    DnDGrab, DndAction, DndGrabHandler, DndTarget, GrabType, Source, SourceMetadata,
};
use smithay::input::pointer::Focus;
use smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction as WlDndAction;
use smithay::utils::{IsAlive, Logical, Point};
use smithay::wayland::selection::data_device::{
    default_action_chooser, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};

impl SelectionHandler for DesktopState {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: smithay::input::Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                focaldesk_logging::flog(&format!("failed to set XWayland selection: {err}"));
            }
        }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: smithay::input::Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                focaldesk_logging::flog(&format!("failed to send XWayland selection: {err}"));
            }
        }
    }
}

#[derive(Clone)]
struct CursorDndSource<S> {
    inner: S,
    phase: Arc<AtomicU8>,
}

impl<S> CursorDndSource<S> {
    fn new(inner: S, phase: Arc<AtomicU8>) -> Self {
        Self { inner, phase }
    }
}

impl<S> Drop for CursorDndSource<S> {
    fn drop(&mut self) {
        self.phase.store(DND_CURSOR_ENDED, Ordering::Relaxed);
    }
}

impl<S: Source> IsAlive for CursorDndSource<S> {
    fn alive(&self) -> bool {
        self.inner.alive()
    }
}

impl<S: Source> Source for CursorDndSource<S> {
    fn is_client_local(&self, target: &dyn std::any::Any) -> bool {
        self.inner.is_client_local(target)
    }

    fn metadata(&self) -> Option<SourceMetadata> {
        self.inner.metadata()
    }

    fn choose_action(&self, action: DndAction) {
        self.phase.store(
            if matches!(action, DndAction::None) {
                DND_CURSOR_INVALID
            } else {
                DND_CURSOR_VALID
            },
            Ordering::Relaxed,
        );
        self.inner.choose_action(action);
    }

    fn send(&self, mime_type: &str, fd: OwnedFd) {
        self.inner.send(mime_type, fd);
    }

    fn drop_performed(&self) {
        self.inner.drop_performed();
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn finished(&self) {
        self.inner.finished();
    }
}

impl WaylandDndGrabHandler for DesktopState {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
        seat: smithay::input::Seat<Self>,
        serial: smithay::utils::Serial,
        type_: GrabType,
    ) {
        let _ = icon;

        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else {
                    source.cancel();
                    return;
                };
                let Some(start_data) = pointer.grab_start_data() else {
                    source.cancel();
                    return;
                };
                let phase = Arc::new(AtomicU8::new(crate::core::desktop::DND_CURSOR_FILE));
                self.begin_dnd_cursor(phase.clone());
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(
                        &self.display_handle,
                        start_data,
                        CursorDndSource::new(source, phase),
                        seat,
                    ),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else {
                    source.cancel();
                    return;
                };
                let Some(start_data) = touch.grab_start_data() else {
                    source.cancel();
                    return;
                };
                let phase = Arc::new(AtomicU8::new(crate::core::desktop::DND_CURSOR_FILE));
                self.begin_dnd_cursor(phase.clone());
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(
                        &self.display_handle,
                        start_data,
                        CursorDndSource::new(source, phase),
                        seat,
                    ),
                    serial,
                );
            }
        }
    }
}
impl DndGrabHandler for DesktopState {
    fn dropped(
        &mut self,
        target: Option<DndTarget<'_, Self>>,
        validated: bool,
        seat: smithay::input::Seat<Self>,
        location: Point<f64, Logical>,
    ) {
        let _ = (target, validated, seat, location);
        self.end_dnd_cursor();
    }
}

impl DataDeviceHandler for DesktopState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }

    fn action_choice(&mut self, available: WlDndAction, preferred: WlDndAction) -> WlDndAction {
        let chosen = default_action_chooser(available, preferred);
        if let Some(phase) = self.dnd_cursor_phase.as_ref() {
            phase.store(
                if chosen.is_empty() {
                    DND_CURSOR_INVALID
                } else {
                    DND_CURSOR_VALID
                },
                Ordering::Relaxed,
            );
        }
        chosen
    }
}

impl PrimarySelectionHandler for DesktopState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

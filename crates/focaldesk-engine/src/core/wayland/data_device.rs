#![allow(unused_imports)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
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
    default_action_chooser, request_data_device_client_selection, set_data_device_selection,
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};

/// Mime types we know how to capture into clipboard history, in preference order.
const TEXT_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Owner of a compositor-provided (server-side) selection, i.e. one set via
/// [`set_data_device_selection`] rather than by a client directly.
#[derive(Clone)]
pub enum ClipboardSelectionOwner {
    /// Bridges the Wayland clipboard to XWayland's X11 selection owner.
    #[cfg(feature = "xwayland")]
    XWayland,
    /// Re-serves a past clipboard-history entry as the live selection.
    ClipboardHistory(Arc<Vec<u8>>),
}

impl SelectionHandler for DesktopState {
    type SelectionUserData = ClipboardSelectionOwner;

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        seat: smithay::input::Seat<Self>,
    ) {
        #[cfg(feature = "xwayland")]
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) =
                xwm.new_selection(ty, source.as_ref().map(|source| source.mime_types()))
            {
                focaldesk_logging::flog(&format!("failed to set XWayland selection: {err}"));
            }
        }

        if ty != SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else {
            return;
        };
        let available = source.mime_types();
        let Some(mime_type) = TEXT_MIME_TYPES
            .iter()
            .find(|candidate| available.iter().any(|m| m == *candidate))
            .map(|m| m.to_string())
        else {
            return;
        };

        // Smithay invokes this callback *before* it records the new selection as
        // current (see `data_device/device.rs`'s `SetSelection` handling), so a
        // synchronous `request_data_device_client_selection` here would still see
        // the previous selection. Defer the actual read to the next dispatch tick
        // via `process_clipboard_captures`, by which point it has been recorded.
        self.clipboard_pending_captures.push(mime_type);
        let _ = seat;
    }

    #[cfg_attr(not(feature = "xwayland"), allow(unused_variables))]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: smithay::input::Seat<Self>,
        user_data: &Self::SelectionUserData,
    ) {
        match user_data {
            ClipboardSelectionOwner::ClipboardHistory(bytes) => {
                let mut file = File::from(fd);
                let _ = file.write_all(bytes);
            }
            #[cfg(feature = "xwayland")]
            ClipboardSelectionOwner::XWayland => {
                if let Some(xwm) = self.xwm.as_mut() {
                    if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                        focaldesk_logging::flog(&format!(
                            "failed to send XWayland selection: {err}"
                        ));
                    }
                }
            }
        }
    }
}

impl DesktopState {
    /// Re-serve a stored clipboard-history entry as the live compositor clipboard selection.
    pub fn restore_clipboard_entry(&mut self, id: u64) {
        let Some(entry) = self.clipboard_history.get(id) else {
            return;
        };
        let mime_type = entry.mime_type.clone();
        let bytes = Arc::new(entry.text.clone().into_bytes());

        set_data_device_selection(
            &self.display_handle,
            &self.seat,
            vec![mime_type],
            ClipboardSelectionOwner::ClipboardHistory(bytes),
        );
    }

    /// Drain mime types queued by [`SelectionHandler::new_selection`] and kick off
    /// a background read of the now-current client selection for each.
    pub(crate) fn begin_pending_clipboard_captures(&mut self) {
        for mime_type in std::mem::take(&mut self.clipboard_pending_captures) {
            let Ok((read_fd, write_fd)) = nix::unistd::pipe() else {
                continue;
            };
            // Safety: `nix::unistd::pipe` returns two freshly-opened, unique fds.
            let read_fd = unsafe { OwnedFd::from_raw_fd(read_fd) };
            let write_fd = unsafe { OwnedFd::from_raw_fd(write_fd) };

            if request_data_device_client_selection(&self.seat, mime_type.clone(), write_fd)
                .is_err()
            {
                continue;
            }

            let tx = self.clipboard_capture_tx.clone();
            std::thread::spawn(move || {
                let mut file = File::from(read_fd);
                let mut buf = Vec::new();
                if file.read_to_end(&mut buf).is_err() {
                    return;
                }
                let text = String::from_utf8_lossy(&buf)
                    .trim_end_matches('\0')
                    .to_string();
                if !text.is_empty() {
                    let _ = tx.send((mime_type, text));
                }
            });
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

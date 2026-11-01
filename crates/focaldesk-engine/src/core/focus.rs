//! Keyboard focus types for [`crate::core::wayland::seat::DesktopState`] so xdg popup grabs
//! (`PopupManager::grab_popup`) can be installed (requires [`KeyboardFocus`]: [`From<PopupKind>`](PopupKind)).

use smithay::backend::input::KeyState;
use smithay::desktop::{PopupKind, Window};
use smithay::input::dnd::{DndFocus, OfferData, Source};
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
};
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Serial};
use smithay::wayland::seat::WaylandFocus;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use tracing::warn;
use wayland_server::DisplayHandle;

use crate::core::desktop::DesktopState;

fn x11_surface(window: &Window) -> Option<&smithay::xwayland::X11Surface> {
    window.x11_surface()
}

#[derive(Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Window(Window),
    Popup(PopupKind),
}

impl fmt::Debug for KeyboardFocusTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyboardFocusTarget::Window(w) => f.debug_tuple("Window").field(w).finish(),
            KeyboardFocusTarget::Popup(p) => f.debug_tuple("Popup").field(p).finish(),
        }
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            KeyboardFocusTarget::Window(w) => w.alive(),
            KeyboardFocusTarget::Popup(p) => p.alive(),
        }
    }
}

impl KeyboardTarget<DesktopState> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::enter(x11, seat, data, keys, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::enter(cow.as_ref(), seat, data, keys, serial);
                    return;
                }
                warn!(?w, "keyboard focus enter skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::enter(p.wl_surface(), seat, data, keys, serial)
            }
        }
    }

    fn leave(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, serial: Serial) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::leave(x11, seat, data, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::leave(cow.as_ref(), seat, data, serial);
                    return;
                }
                warn!(?w, "keyboard focus leave skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::leave(p.wl_surface(), seat, data, serial)
            }
        }
    }

    fn key(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::key(x11, seat, data, key, state, serial, time);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::key(cow.as_ref(), seat, data, key, state, serial, time);
                    return;
                }
                warn!(?w, "keyboard focus key skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::key(p.wl_surface(), seat, data, key, state, serial, time)
            }
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::modifiers(x11, seat, data, modifiers, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::modifiers(cow.as_ref(), seat, data, modifiers, serial);
                    return;
                }
                warn!(
                    ?w,
                    "keyboard focus modifiers skipped: window has no wl_surface"
                );
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::modifiers(p.wl_surface(), seat, data, modifiers, serial)
            }
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            KeyboardFocusTarget::Window(w) => w.wl_surface(),
            KeyboardFocusTarget::Popup(p) => Some(Cow::Borrowed(p.wl_surface())),
        }
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(p: PopupKind) -> Self {
        Self::Popup(p)
    }
}

impl From<Window> for KeyboardFocusTarget {
    fn from(w: Window) -> Self {
        Self::Window(w)
    }
}

impl From<KeyboardFocusTarget> for WlSurface {
    fn from(t: KeyboardFocusTarget) -> Self {
        match t {
            KeyboardFocusTarget::Window(w) => w
                .wl_surface()
                .expect("keyboard focus: wayland window")
                .into_owned(),
            KeyboardFocusTarget::Popup(p) => p.into(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum PointerFocusTarget {
    Wayland(WlSurface),
    #[cfg(feature = "xwayland")]
    Xwayland(smithay::xwayland::X11Surface),
}

impl fmt::Debug for PointerFocusTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                f.debug_tuple("Wayland").field(surface).finish()
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                f.debug_tuple("Xwayland").field(surface).finish()
            }
        }
    }
}

impl IsAlive for PointerFocusTarget {
    fn alive(&self) -> bool {
        match self {
            PointerFocusTarget::Wayland(surface) => surface.alive(),
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => surface.alive(),
        }
    }
}

impl WaylandFocus for PointerFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            PointerFocusTarget::Wayland(surface) => Some(Cow::Borrowed(surface)),
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => surface.wl_surface().map(Cow::Owned),
        }
    }
}

impl PointerTarget<DesktopState> for PointerFocusTarget {
    fn enter(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &MotionEvent) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::enter(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::enter(surface, seat, data, event)
            }
        }
    }

    fn motion(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &MotionEvent) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::motion(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::motion(surface, seat, data, event)
            }
        }
    }

    fn relative_motion(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &RelativeMotionEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::relative_motion(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::relative_motion(surface, seat, data, event)
            }
        }
    }

    fn button(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, event: &ButtonEvent) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::button(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::button(surface, seat, data, event)
            }
        }
    }

    fn axis(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, frame: AxisFrame) {
        match self {
            PointerFocusTarget::Wayland(surface) => PointerTarget::axis(surface, seat, data, frame),
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::axis(surface, seat, data, frame)
            }
        }
    }

    fn frame(&self, seat: &Seat<DesktopState>, data: &mut DesktopState) {
        match self {
            PointerFocusTarget::Wayland(surface) => PointerTarget::frame(surface, seat, data),
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => PointerTarget::frame(surface, seat, data),
        }
    }

    fn leave(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, serial: Serial, time: u32) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::leave(surface, seat, data, serial, time)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::leave(surface, seat, data, serial, time)
            }
        }
    }

    fn gesture_swipe_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeBeginEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_swipe_begin(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_swipe_begin(surface, seat, data, event)
            }
        }
    }

    fn gesture_swipe_update(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeUpdateEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_swipe_update(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_swipe_update(surface, seat, data, event)
            }
        }
    }

    fn gesture_swipe_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureSwipeEndEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_swipe_end(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_swipe_end(surface, seat, data, event)
            }
        }
    }

    fn gesture_pinch_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchBeginEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_pinch_begin(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_pinch_begin(surface, seat, data, event)
            }
        }
    }

    fn gesture_pinch_update(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchUpdateEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_pinch_update(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_pinch_update(surface, seat, data, event)
            }
        }
    }

    fn gesture_pinch_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GesturePinchEndEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_pinch_end(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_pinch_end(surface, seat, data, event)
            }
        }
    }

    fn gesture_hold_begin(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureHoldBeginEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_hold_begin(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_hold_begin(surface, seat, data, event)
            }
        }
    }

    fn gesture_hold_end(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        event: &GestureHoldEndEvent,
    ) {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                PointerTarget::gesture_hold_end(surface, seat, data, event)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                PointerTarget::gesture_hold_end(surface, seat, data, event)
            }
        }
    }
}

pub enum PointerOfferData<S>
where
    S: Source,
{
    Wayland(<WlSurface as DndFocus<DesktopState>>::OfferData<S>),
    #[cfg(feature = "xwayland")]
    Xwayland(<smithay::xwayland::X11Surface as DndFocus<DesktopState>>::OfferData<S>),
}

impl<S> OfferData for PointerOfferData<S>
where
    S: Source,
{
    fn disable(&self) {
        match self {
            PointerOfferData::Wayland(offer) => offer.disable(),
            #[cfg(feature = "xwayland")]
            PointerOfferData::Xwayland(offer) => offer.disable(),
        }
    }

    fn drop(&self) {
        match self {
            PointerOfferData::Wayland(offer) => offer.drop(),
            #[cfg(feature = "xwayland")]
            PointerOfferData::Xwayland(offer) => offer.drop(),
        }
    }

    fn validated(&self) -> bool {
        match self {
            PointerOfferData::Wayland(offer) => offer.validated(),
            #[cfg(feature = "xwayland")]
            PointerOfferData::Xwayland(offer) => offer.validated(),
        }
    }
}

impl DndFocus<DesktopState> for PointerFocusTarget {
    type OfferData<S>
        = PointerOfferData<S>
    where
        S: Source;

    fn enter<S: Source>(
        &self,
        data: &mut DesktopState,
        dh: &DisplayHandle,
        source: Arc<S>,
        seat: &Seat<DesktopState>,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
        serial: &Serial,
    ) -> Option<Self::OfferData<S>> {
        match self {
            PointerFocusTarget::Wayland(surface) => {
                DndFocus::enter(surface, data, dh, source, seat, location, serial)
                    .map(PointerOfferData::Wayland)
            }
            #[cfg(feature = "xwayland")]
            PointerFocusTarget::Xwayland(surface) => {
                DndFocus::enter(surface, data, dh, source, seat, location, serial)
                    .map(PointerOfferData::Xwayland)
            }
        }
    }

    fn motion<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
        time: u32,
    ) {
        match (self, offer) {
            (PointerFocusTarget::Wayland(surface), Some(PointerOfferData::Wayland(offer))) => {
                DndFocus::motion(surface, data, Some(offer), seat, location, time)
            }
            (PointerFocusTarget::Wayland(surface), None) => {
                DndFocus::motion::<S>(surface, data, None, seat, location, time)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), Some(PointerOfferData::Xwayland(offer))) => {
                DndFocus::motion(surface, data, Some(offer), seat, location, time)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), None) => {
                DndFocus::motion::<S>(surface, data, None, seat, location, time)
            }
            _ => {}
        }
    }

    fn leave<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
    ) {
        match (self, offer) {
            (PointerFocusTarget::Wayland(surface), Some(PointerOfferData::Wayland(offer))) => {
                DndFocus::leave(surface, data, Some(offer), seat)
            }
            (PointerFocusTarget::Wayland(surface), None) => {
                DndFocus::leave::<S>(surface, data, None, seat)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), Some(PointerOfferData::Xwayland(offer))) => {
                DndFocus::leave(surface, data, Some(offer), seat)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), None) => {
                DndFocus::leave::<S>(surface, data, None, seat)
            }
            _ => {}
        }
    }

    fn drop<S: Source>(
        &self,
        data: &mut DesktopState,
        offer: Option<&mut Self::OfferData<S>>,
        seat: &Seat<DesktopState>,
    ) {
        match (self, offer) {
            (PointerFocusTarget::Wayland(surface), Some(PointerOfferData::Wayland(offer))) => {
                DndFocus::drop(surface, data, Some(offer), seat)
            }
            (PointerFocusTarget::Wayland(surface), None) => {
                DndFocus::drop::<S>(surface, data, None, seat)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), Some(PointerOfferData::Xwayland(offer))) => {
                DndFocus::drop(surface, data, Some(offer), seat)
            }
            #[cfg(feature = "xwayland")]
            (PointerFocusTarget::Xwayland(surface), None) => {
                DndFocus::drop::<S>(surface, data, None, seat)
            }
            _ => {}
        }
    }
}

impl From<KeyboardFocusTarget> for PointerFocusTarget {
    fn from(target: KeyboardFocusTarget) -> Self {
        match target {
            KeyboardFocusTarget::Window(window) => {
                #[cfg(feature = "xwayland")]
                if let Some(surface) = x11_surface(&window) {
                    return PointerFocusTarget::Xwayland(surface.clone());
                }

                PointerFocusTarget::Wayland(
                    window
                        .wl_surface()
                        .expect("pointer focus: window without surface")
                        .into_owned(),
                )
            }
            KeyboardFocusTarget::Popup(popup) => {
                PointerFocusTarget::Wayland(popup.wl_surface().clone())
            }
        }
    }
}

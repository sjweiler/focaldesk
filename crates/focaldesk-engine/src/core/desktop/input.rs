use super::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedKeyEvent {
    pub(super) keycode: u32,
    pub(super) state: KeyState,
    pub(super) smithay_state: smithay::backend::input::KeyState,
    pub(super) serial: Serial,
    pub(super) time: u32,
    pub(super) modifiers_changed: bool,
    pub(super) symbol: u32,
    pub(super) modifiers: KeyboardModifiers,
}

impl PreparedKeyEvent {
    pub(crate) fn input_event(self) -> InputEvent {
        InputEvent::Key {
            keycode: self.keycode,
            state: self.state,
            repeat: false,
            modifiers: self.modifiers.input,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeyboardModifiers {
    pub input: Modifiers,
    pub shortcuts: ModMask,
}

/// Translate Smithay's keyboard state once at the engine boundary so the
/// input and shortcut crates share the same backend-neutral modifier snapshot.
pub(super) fn keyboard_modifiers(
    modifiers: &smithay::input::keyboard::ModifiersState,
) -> KeyboardModifiers {
    let input = Modifiers {
        shift: modifiers.shift,
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        super_key: modifiers.logo,
    };

    let mut shortcuts = ModMask::empty();
    if input.shift {
        shortcuts |= ModMask::SHIFT;
    }
    if input.ctrl {
        shortcuts |= ModMask::CTRL;
    }
    if input.alt {
        shortcuts |= ModMask::ALT;
    }
    if input.super_key {
        shortcuts |= ModMask::SUPER;
    }

    KeyboardModifiers { input, shortcuts }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputTarget {
    LockScreen,
    Desktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InputRoute {
    target: InputTarget,
    record_user_activity: bool,
}

fn input_route(lock_screen_active: bool) -> InputRoute {
    if lock_screen_active {
        InputRoute {
            target: InputTarget::LockScreen,
            record_user_activity: false,
        }
    } else {
        InputRoute {
            target: InputTarget::Desktop,
            record_user_activity: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientButtonDelivery {
    Forward,
    FinishGrab,
    Suppress,
}

fn client_button_delivery(
    compositor_grab_active: bool,
    button: MouseButton,
    state: KeyState,
) -> ClientButtonDelivery {
    match (compositor_grab_active, button, state) {
        (false, _, _) => ClientButtonDelivery::Forward,
        (true, MouseButton::Left, KeyState::Released) => ClientButtonDelivery::FinishGrab,
        (true, _, _) => ClientButtonDelivery::Suppress,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShortcutDispatch {
    Forward,
    Intercept,
    Trigger(KeyAction),
}

/// Resolve compositor shortcuts once per key cycle: trigger on press, consume
/// the matching release, and forward keys that are not bound in this session.
pub(super) fn shortcut_dispatch(
    keybinds: &Keybinds,
    sym: KeySymbol,
    mods: ModMask,
    state: KeyState,
) -> ShortcutDispatch {
    let Some(action) = keybinds.resolve(sym, mods) else {
        return ShortcutDispatch::Forward;
    };

    match state {
        KeyState::Pressed => ShortcutDispatch::Trigger(action),
        KeyState::Released => ShortcutDispatch::Intercept,
    }
}

fn workspace_swipe_target(
    current: u32,
    last: u32,
    delta_x: f64,
    delta_y: f64,
    velocity_x: f64,
    velocity_y: f64,
    activation_distance: f64,
    cancelled: bool,
) -> Option<u32> {
    let distance_commit = delta_x.abs() >= activation_distance;
    let velocity_commit = velocity_x.abs() >= 0.65 && velocity_x.abs() >= velocity_y.abs() * 1.25;
    if cancelled || (!distance_commit && !velocity_commit) || delta_x.abs() < delta_y.abs() * 1.25 {
        return None;
    }
    let target = if delta_x < 0.0 {
        current.saturating_add(1).min(last)
    } else {
        current.saturating_sub(1).max(1)
    };
    (target != current).then_some(target)
}

impl DesktopState {
    /// Advance Smithay's XKB state before any compositor overlay inspects the
    /// key. The returned event can then be intercepted or dispatched exactly
    /// once without applying the key transition a second time.
    pub(crate) fn prepare_key_event(
        &mut self,
        keycode: u32,
        state: KeyState,
    ) -> Option<PreparedKeyEvent> {
        use smithay::backend::input::KeyState as SmithayKeyState;

        let smithay_state = match state {
            KeyState::Pressed => SmithayKeyState::Pressed,
            KeyState::Released => SmithayKeyState::Released,
        };
        let keyboard = self.seat.get_keyboard()?;
        let ((symbol, modifiers), modifiers_changed) = keyboard.input_intercept(
            self,
            keycode.into(),
            smithay_state,
            |ds, modifiers, handle| {
                let modifiers = keyboard_modifiers(modifiers);
                ds.input.modifiers = modifiers.input;
                (handle.modified_sym().raw(), modifiers)
            },
        );

        Some(PreparedKeyEvent {
            keycode,
            state,
            smithay_state,
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
            modifiers_changed,
            symbol,
            modifiers,
        })
    }

    pub(crate) fn finish_external_key_event(
        &mut self,
        prepared: PreparedKeyEvent,
        intercepted: bool,
    ) {
        if self.debug.show_input_events {
            focaldesk_logging::logging::flog(
                FLogLevel::Debug,
                format!("input event: {:?}", prepared.input_event()),
            );
        }
        self.record_user_activity();
        if !intercepted {
            self.dispatch_prepared_key_event(prepared);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
    }

    pub fn handle_input(&mut self, event: InputEvent) {
        if self.debug.show_input_events {
            focaldesk_logging::logging::flog(FLogLevel::Debug, format!("input event: {event:?}"));
        }

        let route = input_route(self.lock_screen.active);
        if route.record_user_activity {
            self.record_user_activity();
        }

        if route.target == InputTarget::LockScreen {
            self.handle_lock_screen_input(event);
            return;
        }

        match event {
            event @ InputEvent::Key { .. } => self.handle_physical_keyboard_input(event),

            event @ InputEvent::PointerMoved { .. } => self.handle_pointer_motion(event),

            event @ InputEvent::PointerButton { .. } => self.handle_pointer_button(event),
            event @ InputEvent::PointerScroll { .. } => self.handle_pointer_scroll(event),
            InputEvent::GestureSwipeBegin { fingers, time_msec } => {
                self.handle_gesture_swipe_begin(fingers, time_msec)
            }
            InputEvent::GestureSwipeUpdate { delta, time_msec } => {
                self.handle_gesture_swipe_update(delta, time_msec)
            }
            InputEvent::GestureSwipeEnd {
                cancelled,
                time_msec,
            } => self.handle_gesture_swipe_end(cancelled, time_msec),
            InputEvent::PointerEntered => self.handle_pointer_entered(),
            InputEvent::PointerLeft => self.handle_pointer_left(),
        }
    }

    fn handle_lock_screen_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key { keycode, state, .. } => {
                self.handle_lock_key_event(keycode, state);
            }
            InputEvent::PointerMoved { position, .. } => {
                self.input.pointer_pos = position;
                let position = position.into();
                self.pointer_pos = position;
                self.update_magnifier_pointer_focus(position);
                self.cursor_manager.move_to(position.x, position.y);
                self.clear_client_pointer_focus(position);
            }
            InputEvent::PointerButton {
                button,
                state,
                position,
            } => {
                self.input.pointer_pos = position;
                let position = position.into();
                self.pointer_pos = position;
                self.update_magnifier_pointer_focus(position);
                self.cursor_manager.move_to(position.x, position.y);
                self.clear_client_pointer_focus(position);
                if matches!((button, state), (MouseButton::Left, KeyState::Pressed)) {
                    self.toggle_lock_password_visibility_at(position);
                }
            }
            InputEvent::PointerScroll { position, .. } => {
                self.input.pointer_pos = position;
                let position = position.into();
                self.pointer_pos = position;
                self.update_magnifier_pointer_focus(position);
                self.cursor_manager.move_to(position.x, position.y);
                self.clear_client_pointer_focus(position);
            }
            InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. } => {}
            InputEvent::PointerEntered => {
                self.cursor_manager.set_visible(true);
            }
            InputEvent::PointerLeft => {
                self.cursor_manager.set_visible(false);
            }
        }

        self.pending_compositor_move = None;
        self.pending_xdg_move = None;
        self.toplevel_pointer = None;
        self.split_divider_drag = None;
        self.input.pointer_left_down = false;
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    fn handle_pointer_button(&mut self, event: InputEvent) {
        let InputEvent::PointerButton {
            button,
            state,
            position,
        } = &event
        else {
            unreachable!("pointer-button handler received another event");
        };
        let (button, state, position) = (*button, *state, *position);

        if matches!(state, KeyState::Pressed) && self.ui.focused.is_some() {
            self.ui.clear_focus();
            self.publish_accessibility_tree();
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
        self.input.pointer_pos = position;
        let position = position.into();
        self.pointer_pos = position;

        if let Some(id) = self.output_under_pointer(position) {
            self.set_focused_output(id);
        }
        self.update_magnifier_pointer_focus(position);
        // Rendering rebuilds the compositor's single UiTree once per
        // output. Resolve this click against the output that actually
        // received it, not whichever output happened to render last.
        self.rebuild_ui_tree_for_output(self.focused_output);
        let cursor_owner_damage = self.update_cursor_owner_damage();
        let stale_cursor_damage = self.clear_stale_software_cursor_damage();

        if self.pointer_button_consumed_by_overlay(&event) {
            return;
        }
        if self.handle_split_divider_button(button, state, position) {
            return;
        }
        if self.handle_shell_pointer_button(button, state, position) {
            return;
        }
        self.forward_pointer_button_to_desktop(
            button,
            state,
            position,
            cursor_owner_damage,
            stale_cursor_damage,
        );
    }

    fn pointer_button_consumed_by_overlay(&mut self, event: &InputEvent) -> bool {
        if self.egui_open_on_output(self.focused_output) {
            let _ = self.handle_egui_input(event);
        } else if self.handle_egui_input(event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_egui_damage(self.focused_output);
            return true;
        }

        if self.handle_dialog_input(event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return true;
        }

        false
    }

    fn handle_split_divider_button(
        &mut self,
        button: MouseButton,
        state: KeyState,
        position: Point<f64, Logical>,
    ) -> bool {
        if matches!(button, MouseButton::Right) && matches!(state, KeyState::Pressed) {
            if let Some(divider) = self.split_divider_at(position) {
                if !self.focused_window.is_some_and(|id| {
                    divider.before.contains(&Some(id)) || divider.after.contains(&Some(id))
                }) {
                    if let Some(id) = divider
                        .before
                        .into_iter()
                        .chain(divider.after)
                        .flatten()
                        .next()
                    {
                        self.focus_window_id(id);
                    }
                }
                self.dispatch_ui_action(UiAction::OpenPanel(PanelKind::SplitGroup));
                self.clear_client_pointer_focus(position);
                self.mark_output_full_damage(divider.output, DamageSource::Egui);
                return true;
            }
        }

        if matches!(button, MouseButton::Left) {
            match state {
                KeyState::Pressed => {
                    if let Some(divider) = self.split_divider_at(position) {
                        self.input.pointer_left_down = true;
                        self.record_split_undo(
                            divider.before.into_iter().chain(divider.after).flatten(),
                        );
                        self.split_divider_drag = Some(SplitDividerDrag { divider });
                        self.pending_compositor_move = None;
                        self.pending_xdg_move = None;
                        self.clear_client_pointer_focus(position);
                        self.update_pointer_cursor(position);
                        self.mark_output_full_damage(divider.output, DamageSource::WindowResize);
                        return true;
                    }
                }
                KeyState::Released => {
                    if let Some(drag) = self.split_divider_drag.take() {
                        if let Some(pair) = self.split_pair_for_output(drag.divider.output) {
                            self.remember_split_ratio(pair);
                        }
                        self.input.pointer_left_down = false;
                        self.clear_client_pointer_focus(position);
                        self.update_pointer_cursor(position);
                        self.mark_output_full_damage(
                            drag.divider.output,
                            DamageSource::WindowResize,
                        );
                        return true;
                    }
                }
            }
        }

        false
    }

    fn handle_shell_pointer_button(
        &mut self,
        button: MouseButton,
        state: KeyState,
        position: Point<f64, Logical>,
    ) -> bool {
        if matches!(button, MouseButton::Left)
            && matches!(state, KeyState::Released)
            && self.suppress_next_left_release
        {
            self.suppress_next_left_release = false;
            self.input.pointer_left_down = false;
            self.pending_compositor_move = None;
            self.pending_xdg_move = None;
            self.update_pointer_cursor(position);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return true;
        }

        if matches!(button, MouseButton::Left) && self.peek_ui_action_at_pointer().is_some() {
            let mut damaged_precisely = false;
            match state {
                KeyState::Pressed => {
                    self.input.pointer_left_down = true;
                    let pressed_element = self
                        .ui_element_at_pointer_for_output(self.focused_output)
                        .map(|el| (el.id, el.kind));
                    self.ui.pressed = pressed_element.map(|(id, _)| id);
                    self.clear_client_pointer_focus(position);
                    damaged_precisely = match pressed_element {
                        Some((_, UiElementKind::SidebarButton)) => {
                            self.trigger_sidebar_pulse_at_pointer(self.focused_output)
                        }
                        Some((_, UiElementKind::TopbarIndicator | UiElementKind::TopbarButton)) => {
                            self.trigger_topbar_pulse_at_pointer(self.focused_output)
                        }
                        Some((_, UiElementKind::Clock)) => {
                            self.trigger_clock_pulse_at_pointer(self.focused_output)
                        }
                        _ => false,
                    };
                }
                KeyState::Released => {
                    self.input.pointer_left_down = false;
                    if let Some(pressed_id) = self.ui.pressed.take() {
                        let clicked = self
                            .ui_element_at_pointer_for_output(self.focused_output)
                            .is_some_and(|el| el.id == pressed_id && el.enabled);
                        if clicked && self.click_ui_at_pointer() {
                            self.mark_focused_output_full_damage(DamageSource::Unknown);
                        }
                    }
                    self.pending_compositor_move = None;
                }
            }
            self.update_pointer_cursor(position);
            if !damaged_precisely
                && !matches!(state, KeyState::Released)
                && self.sidebar_pulse.is_none()
                && self.topbar_pulse.is_none()
                && self.clock_pulse.is_none()
            {
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }
            return true;
        }

        if matches!(button, MouseButton::Left)
            && matches!(state, KeyState::Released)
            && self.ui.pressed.is_some()
        {
            self.input.pointer_left_down = false;
            self.ui.pressed = None;
            self.pending_compositor_move = None;
            self.clear_client_pointer_focus(position);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return true;
        }

        if self.egui_open_on_output(self.focused_output)
            && matches!(button, MouseButton::Left)
            && matches!(state, KeyState::Pressed | KeyState::Released)
        {
            self.process_egui_actions();
            if matches!(state, KeyState::Released)
                && self.egui_wants_pointer_on_output(self.focused_output)
            {
                self.clear_client_pointer_focus(self.pointer_pos);
                self.mark_egui_damage(self.focused_output);
                return true;
            }
        }

        false
    }

    fn forward_pointer_button_to_desktop(
        &mut self,
        button: MouseButton,
        state: KeyState,
        position: Point<f64, Logical>,
        cursor_owner_damage: bool,
        stale_cursor_damage: bool,
    ) {
        if matches!(button, MouseButton::Left) {
            match state {
                KeyState::Pressed => {
                    self.input.pointer_left_down = true;
                }
                KeyState::Released => {
                    self.input.pointer_left_down = false;
                    self.pending_compositor_move = None;
                    self.pending_xdg_move = None;
                }
            }
        }

        if let Some(id) = self.output_under_pointer(position) {
            self.set_focused_output(id);
        }
        self.cursor_manager.move_to(position.x, position.y);
        if self.has_workspace_transition(self.focused_output) {
            self.update_pointer_cursor(position);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return;
        }
        let precise_toplevel_damage = self.process_toplevel_pointer_motion(position);
        let precise_hover_damage = self.update_ui_hover_for_output(self.focused_output);
        if matches!(state, KeyState::Pressed) {
            if matches!(button, MouseButton::Left)
                && self.pointer_on_chrome_host_drag_region(position)
            {
                self.host_window_drag_requested = true;
                self.pending_compositor_move = None;
            } else if matches!(button, MouseButton::Left)
                && self.handle_xwayland_titlebar_press(position)
            {
                self.clear_client_pointer_focus(position);
                self.update_pointer_cursor(position);
                self.mark_focused_output_full_damage(DamageSource::Unknown);
                return;
            } else {
                self.focus_window_at(position);
                if matches!(button, MouseButton::Left) && self.pointer_in_work_recess(position) {
                    if let Some((id, edges)) = self.top_window_resize_edge_at(position) {
                        self.pending_compositor_move = None;
                        if let Ok(edge) = ResizeEdge::try_from(edges) {
                            self.try_begin_compositor_resize(id, edge);
                        }
                    }
                }
            }
        }
        match client_button_delivery(self.compositor_pointer_grab_active(), button, state) {
            ClientButtonDelivery::FinishGrab => {
                self.process_toplevel_pointer_button(button, state);
                self.forward_pointer_button(position, button, state);
            }
            ClientButtonDelivery::Forward => {
                self.forward_pointer_to_clients(position);
                self.forward_pointer_button(position, button, state);
                self.process_toplevel_pointer_button(button, state);
            }
            ClientButtonDelivery::Suppress => {}
        }
        self.update_pointer_cursor(position);
        let precise_cursor_damage =
            self.software_cursor_damage_pending_for_output(self.focused_output);
        if !precise_toplevel_damage
            && !precise_hover_damage
            && !precise_cursor_damage
            && !stale_cursor_damage
            && !cursor_owner_damage
        {
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
    }

    fn handle_keyboard_input(&mut self, event: InputEvent) {
        let InputEvent::Key { keycode, state, .. } = &event else {
            unreachable!("keyboard handler received a non-key event");
        };
        let (keycode, state) = (*keycode, *state);

        let Some(prepared) = self.prepare_key_event(keycode, state) else {
            flog_info!("no keyboard on seat");
            return;
        };
        let event = prepared.input_event();

        if matches!(state, KeyState::Pressed)
            && keycode == 1
            && self.egui_open_on_output(self.focused_output)
        {
            if let Some(output) = self.desktop_outputs.get_mut(&self.focused_output) {
                output.egui.close_all_panels();
            }
            self.pending_split_assist = None;
            self.mark_egui_damage(self.focused_output);
            return;
        }
        if self.handle_egui_input(&event) {
            self.mark_egui_damage(self.focused_output);
            return;
        }
        self.dispatch_prepared_key_event(prepared);
        self.mark_focused_output_full_damage(DamageSource::Unknown);
    }

    fn handle_physical_keyboard_input(&mut self, event: InputEvent) {
        let InputEvent::Key { keycode, state, .. } = event else {
            unreachable!("physical keyboard handler received a non-key event");
        };
        for transition in self.filter_physical_key_event(keycode, state) {
            self.dispatch_accessible_key_transition(transition);
        }
    }

    pub(crate) fn filter_physical_key_event(
        &mut self,
        keycode: u32,
        state: KeyState,
    ) -> Vec<crate::core::keyboard_accessibility::KeyTransition> {
        let role = self.keyboard_accessibility_key_role(keycode);
        let output = self
            .keyboard_accessibility
            .process(keycode, state, role, Instant::now());
        self.apply_keyboard_accessibility_feedback(output.feedback);
        output.events
    }

    pub(crate) fn tick_keyboard_accessibility(&mut self) {
        if self.lock_screen.active {
            return;
        }
        let output = self.keyboard_accessibility.tick(Instant::now());
        self.apply_keyboard_accessibility_feedback(output.feedback);
        for transition in output.events {
            self.dispatch_accessible_key_transition(transition);
        }
    }

    fn apply_keyboard_accessibility_feedback(
        &mut self,
        feedback_events: Vec<crate::core::keyboard_accessibility::Feedback>,
    ) {
        use crate::core::keyboard_accessibility::Feedback;

        for feedback in feedback_events {
            match feedback {
                Feedback::StickyShortcutRequested => {
                    self.open_sticky_keys_shortcut_dialog();
                }
                Feedback::ModifierLatched if self.input_settings.accessibility.visual_feedback => {
                    notification_service_notify(
                        "Sticky Keys",
                        "Modifier latched for the next key",
                        Some(Duration::from_secs(2)),
                    );
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                }
                Feedback::ModifierReleased if self.input_settings.accessibility.visual_feedback => {
                    notification_service_notify(
                        "Sticky Keys",
                        "Latched modifier released",
                        Some(Duration::from_secs(2)),
                    );
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                }
                Feedback::ModifierLatched | Feedback::ModifierReleased => {}
            }
        }
    }

    pub(crate) fn dispatch_accessible_key_transition(
        &mut self,
        transition: crate::core::keyboard_accessibility::KeyTransition,
    ) {
        self.handle_keyboard_input(InputEvent::Key {
            keycode: transition.keycode,
            state: transition.state,
            repeat: false,
            modifiers: self.input.modifiers,
        });
    }

    fn keyboard_accessibility_key_role(
        &self,
        keycode: u32,
    ) -> crate::core::keyboard_accessibility::KeyRole {
        use crate::core::keyboard_accessibility::KeyRole;

        // libinput/Smithay key codes carry XKB's +8 offset. Winit's physical
        // scancodes are raw evdev values, so normalize both backends here.
        let evdev = if self.backend_kind == CompositorBackend::Drm {
            keycode.saturating_sub(8)
        } else {
            keycode
        };
        match evdev {
            42 | 54 => KeyRole::Shift,
            29 | 56 | 97 | 100 | 125 | 126 => KeyRole::Modifier,
            _ => KeyRole::Other,
        }
    }

    fn open_sticky_keys_shortcut_dialog(&mut self) {
        if self.active_dialog.is_some() {
            return;
        }
        let enabling = !self.input_settings.accessibility.sticky_keys;
        let dialog_id = self.alloc_dialog_id();
        let dialog = Dialog {
            id: dialog_id,
            kind: DialogKind::Confirm,
            title: "Sticky Keys shortcut".into(),
            message: if enabling {
                "You pressed Shift five times. Enable Sticky Keys?"
            } else {
                "You pressed Shift five times. Disable Sticky Keys?"
            }
            .into(),
            buttons: vec![
                DialogButton {
                    label: "Cancel".into(),
                    action: DialogAction::Cancel,
                },
                DialogButton {
                    label: if enabling { "Enable" } else { "Disable" }.into(),
                    action: DialogAction::Confirm,
                },
            ],
            modal: true,
            dismissible: true,
            state: DialogState::Open,
            owner_output: self.focused_output,
            bounds: self.focused_output_screen(),
        };
        self.pending_sidebar_dialogs
            .insert(dialog_id, SidebarDialogKind::ToggleStickyKeys);
        self.open_dialog(dialog);
    }

    fn handle_pointer_motion(&mut self, event: InputEvent) {
        let InputEvent::PointerMoved {
            position,
            delta,
            delta_unaccel,
        } = &event
        else {
            unreachable!("pointer-motion handler received another event");
        };
        let (position, delta, delta_unaccel) = (*position, *delta, *delta_unaccel);

        let previous_pos = self.pointer_pos;
        let pointer_locked = self.pointer_lock_active_at(previous_pos);
        let position = self.constrained_pointer_position(previous_pos, position.into());
        let position = self.confine_pointer_to_outputs(previous_pos, position);
        self.input.pointer_pos = position.into();
        self.pointer_pos = position;
        if let Some(id) = self.output_under_pointer(position) {
            self.set_focused_output(id);
        }
        self.update_magnifier_pointer_focus(position);
        // Keep the visible cursor in lockstep with the compositor's logical pointer before an
        // overlay gets a chance to consume the event.
        self.cursor_manager.move_to(position.x, position.y);
        self.drm_submit_hw_cursor = true;
        let cursor_owner_damage = self.update_cursor_owner_damage();
        let stale_cursor_damage = self.clear_stale_software_cursor_damage();
        if self.egui_open_on_output(self.focused_output) {
            let _ = self.handle_egui_input(&event);
            if self.egui_wants_pointer_on_output(self.focused_output) {
                self.clear_client_pointer_focus(self.pointer_pos);
                self.mark_egui_damage(self.focused_output);
                return;
            }
        } else if self.handle_egui_input(&event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_egui_damage(self.focused_output);
            return;
        }
        if self.handle_dialog_input(&event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return;
        }
        if self.input.pointer_left_down {
            if let Some(drag) = self.split_divider_drag {
                self.resize_split_grid_divider(drag.divider, position);
                self.clear_client_pointer_focus(position);
                self.update_pointer_cursor(position);
                self.mark_output_full_damage(drag.divider.output, DamageSource::WindowResize);
                return;
            }
        }
        const DRAG_THRESHOLD_SQ: f64 = 5.0 * 5.0;
        if self.input.pointer_left_down {
            if let Some((id, start)) = self.pending_xdg_move {
                let d = position - start;
                if d.x * d.x + d.y * d.y >= DRAG_THRESHOLD_SQ {
                    self.pending_xdg_move = None;
                    self.request_move(id);
                }
            }
            if let Some((id, start)) = self.pending_compositor_move {
                let d = position - start;
                if d.x * d.x + d.y * d.y >= DRAG_THRESHOLD_SQ {
                    self.pending_compositor_move = None;
                    self.try_begin_compositor_move(id);
                }
            }
        } else {
            self.pending_xdg_move = None;
            if matches!(
                self.toplevel_pointer,
                Some(ToplevelPointerInteraction::Move { .. })
            ) {
                self.toplevel_pointer = None;
                self.forward_pointer_to_clients(self.pointer_pos);
            }
        }
        let precise_toplevel_damage = self.process_toplevel_pointer_motion(position);
        let precise_hover_damage = self.update_ui_hover_for_output(self.focused_output);
        self.update_pointer_cursor(position);
        if !self.compositor_pointer_grab_active() {
            if let (Some(delta), Some(delta_unaccel)) = (delta, delta_unaccel) {
                self.forward_pointer_relative_motion(position, delta.into(), delta_unaccel.into());
            }
            if pointer_locked {
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.frame(self);
                }
            } else {
                // Relative motion belongs to the same logical event and must precede the
                // wl_pointer frame emitted here.
                self.forward_pointer_to_clients(position);
                self.activate_pointer_constraint_at(position);
            }
        }
        let precise_cursor_damage =
            self.software_cursor_damage_pending_for_output(self.focused_output);
        if !precise_toplevel_damage
            && !precise_hover_damage
            && !precise_cursor_damage
            && !stale_cursor_damage
            && !cursor_owner_damage
        {
            // A hardware cursor still needs a render/present cycle so KMS can move its cursor
            // plane, but the primary plane has not visually changed.
            self.mark_output_damage_source(
                self.focused_output,
                Rectangle::from_loc_and_size((0, 0), (1, 1)),
                DamageSource::Cursor,
            );
        }
    }

    fn handle_pointer_scroll(&mut self, event: InputEvent) {
        let InputEvent::PointerScroll { position, delta } = &event else {
            unreachable!("scroll handler received a non-scroll event");
        };
        let (position, delta) = (*position, *delta);

        self.input.pointer_pos = position;
        let position = position.into();
        self.pointer_pos = position;
        if let Some(id) = self.output_under_pointer(position) {
            self.set_focused_output(id);
        }
        self.update_cursor_owner_damage();
        self.clear_stale_software_cursor_damage();
        if self.egui_open_on_output(self.focused_output) {
            let _ = self.handle_egui_input(&event);
            if self.egui_wants_pointer_on_output(self.focused_output) {
                self.clear_client_pointer_focus(self.pointer_pos);
                self.mark_egui_damage(self.focused_output);
                return;
            }
        } else if self.handle_egui_input(&event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_egui_damage(self.focused_output);
            return;
        }
        if self.handle_dialog_input(&event) {
            self.clear_client_pointer_focus(self.pointer_pos);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
            return;
        }
        self.cursor_manager.move_to(position.x, position.y);
        if !self.compositor_pointer_grab_active() {
            self.forward_pointer_to_clients(position);
            self.forward_pointer_scroll(delta);
        }
        if self.egui_open_on_output(self.focused_output) {
            self.mark_egui_damage(self.focused_output);
        } else {
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
    }

    fn handle_gesture_swipe_begin(&mut self, fingers: u32, time_msec: u32) {
        let captured = self.input_settings.workspace_swipe_fingers != 0
            && fingers == self.input_settings.workspace_swipe_fingers;
        if captured {
            self.forget_workspace_transition(self.focused_output);
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
        self.workspace_swipe = Some(WorkspaceSwipe {
            captured,
            delta_x: 0.0,
            delta_y: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            last_update_msec: time_msec,
        });
        if !captured {
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.gesture_swipe_begin(
                    self,
                    &GestureSwipeBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: time_msec,
                        fingers,
                    },
                );
            }
        }
    }

    fn handle_gesture_swipe_update(&mut self, delta: focaldesk_input::Delta, time_msec: u32) {
        let forward = self.workspace_swipe.is_some_and(|swipe| !swipe.captured);
        let mut interactive_delta = None;
        if let Some(swipe) = self.workspace_swipe.as_mut() {
            swipe.delta_x += delta.x;
            swipe.delta_y += delta.y;
            let elapsed_msec = time_msec.wrapping_sub(swipe.last_update_msec);
            if elapsed_msec > 0 {
                let elapsed = f64::from(elapsed_msec);
                let velocity_x = delta.x / elapsed;
                let velocity_y = delta.y / elapsed;
                if swipe.velocity_x == 0.0 && swipe.velocity_y == 0.0 {
                    swipe.velocity_x = velocity_x;
                    swipe.velocity_y = velocity_y;
                } else {
                    swipe.velocity_x = swipe.velocity_x * 0.55 + velocity_x * 0.45;
                    swipe.velocity_y = swipe.velocity_y * 0.55 + velocity_y * 0.45;
                }
            }
            swipe.last_update_msec = time_msec;
            if swipe.captured {
                interactive_delta = Some((swipe.delta_x, swipe.delta_y));
            }
        }
        if let Some((delta_x, delta_y)) = interactive_delta {
            self.update_interactive_workspace_transition(delta_x, delta_y);
        }
        if forward {
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.gesture_swipe_update(
                    self,
                    &GestureSwipeUpdateEvent {
                        time: time_msec,
                        delta: (delta.x, delta.y).into(),
                    },
                );
            }
        }
    }

    fn handle_gesture_swipe_end(&mut self, cancelled: bool, time_msec: u32) {
        let Some(swipe) = self.workspace_swipe.take() else {
            return;
        };
        if !swipe.captured {
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.gesture_swipe_end(
                    self,
                    &GestureSwipeEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: time_msec,
                        cancelled,
                    },
                );
            }
            return;
        }
        let current = self.focused_workspace().0;
        let last = self.workspace_names.len() as u32;
        let target = workspace_swipe_target(
            current,
            last,
            swipe.delta_x,
            swipe.delta_y,
            swipe.velocity_x,
            swipe.velocity_y,
            self.input_settings.workspace_swipe_distance,
            cancelled,
        )
        .map(WorkspaceId);
        self.settle_interactive_workspace_transition(target);
    }

    fn handle_pointer_entered(&mut self) {
        self.cursor_manager.set_visible(true);
        self.update_cursor_owner_damage();
        self.mark_focused_output_full_damage(DamageSource::Cursor);
    }

    fn handle_pointer_left(&mut self) {
        let _ = self.handle_egui_input(&InputEvent::PointerLeft);
        self.process_toplevel_pointer_button(MouseButton::Left, KeyState::Released);
        self.input.pointer_left_down = false;
        self.pending_compositor_move = None;
        self.pending_xdg_move = None;
        self.split_divider_drag = None;
        self.cursor_manager.set_visible(false);
        self.update_cursor_owner_damage();
        self.clear_all_software_cursor_damage();
        self.mark_focused_output_full_damage(DamageSource::Cursor);
    }

    pub fn handle_backend_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::OutputResized {
                output_id,
                width,
                height,
                scale_factor,
            } => {
                let size = Size::<i32, Physical>::from((width as i32, height as i32));
                self.update_output_size(output_id, size, scale_factor);
            }
            BackendEvent::CloseRequested => {
                self.running = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focaldesk_shortcuts::ShortcutCapabilities;
    use smithay::backend::input::KeyState as SmithayKeyState;
    use smithay::input::keyboard::{keysyms, ModifiersState};

    fn prepared_key_event(state: KeyState, modifiers: Modifiers) -> PreparedKeyEvent {
        PreparedKeyEvent {
            keycode: 30,
            state,
            smithay_state: match state {
                KeyState::Pressed => SmithayKeyState::Pressed,
                KeyState::Released => SmithayKeyState::Released,
            },
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
            modifiers_changed: true,
            symbol: keysyms::KEY_a,
            modifiers: KeyboardModifiers {
                input: modifiers,
                shortcuts: ModMask::empty(),
            },
        }
    }

    #[test]
    fn workspace_swipes_require_distance_and_horizontal_intent() {
        assert_eq!(
            workspace_swipe_target(2, 4, -140.0, 20.0, 0.0, 0.0, 120.0, false),
            Some(3)
        );
        assert_eq!(
            workspace_swipe_target(2, 4, 140.0, 20.0, 0.0, 0.0, 120.0, false),
            Some(1)
        );
        assert_eq!(
            workspace_swipe_target(2, 4, -100.0, 10.0, 0.0, 0.0, 120.0, false),
            None
        );
        assert_eq!(
            workspace_swipe_target(2, 4, -140.0, 130.0, 0.0, 0.0, 120.0, false),
            None
        );
        assert_eq!(
            workspace_swipe_target(2, 4, -140.0, 20.0, 0.0, 0.0, 120.0, true),
            None
        );
    }

    #[test]
    fn workspace_swipes_stop_at_workspace_edges() {
        assert_eq!(
            workspace_swipe_target(1, 4, 140.0, 0.0, 0.0, 0.0, 120.0, false),
            None
        );
        assert_eq!(
            workspace_swipe_target(4, 4, -140.0, 0.0, 0.0, 0.0, 120.0, false),
            None
        );
    }

    #[test]
    fn workspace_swipes_accept_a_fast_horizontal_fling() {
        assert_eq!(
            workspace_swipe_target(2, 4, -55.0, 4.0, -0.8, 0.05, 120.0, false),
            Some(3)
        );
    }

    #[test]
    fn prepared_key_sequence_exposes_current_press_and_release_modifiers() {
        let sequence = [
            (KeyState::Pressed, Modifiers::default()),
            (
                KeyState::Pressed,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
            ),
            (
                KeyState::Pressed,
                Modifiers {
                    shift: true,
                    ctrl: true,
                    ..Default::default()
                },
            ),
            (
                KeyState::Released,
                Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            ),
            (KeyState::Released, Modifiers::default()),
        ];

        for (state, expected_modifiers) in sequence {
            assert_eq!(
                prepared_key_event(state, expected_modifiers).input_event(),
                InputEvent::Key {
                    keycode: 30,
                    state,
                    repeat: false,
                    modifiers: expected_modifiers,
                }
            );
        }
    }

    #[test]
    fn keyboard_modifier_adapter_maps_every_supported_combination() {
        for bits in 0_u8..16 {
            let modifiers = ModifiersState {
                shift: bits & 0b0001 != 0,
                ctrl: bits & 0b0010 != 0,
                alt: bits & 0b0100 != 0,
                logo: bits & 0b1000 != 0,
                ..Default::default()
            };
            let mapped = keyboard_modifiers(&modifiers);

            assert_eq!(
                mapped.input,
                Modifiers {
                    shift: modifiers.shift,
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    super_key: modifiers.logo,
                },
                "input modifier mismatch for combination {bits:04b}"
            );

            let mut expected_shortcuts = ModMask::empty();
            if modifiers.shift {
                expected_shortcuts |= ModMask::SHIFT;
            }
            if modifiers.ctrl {
                expected_shortcuts |= ModMask::CTRL;
            }
            if modifiers.alt {
                expected_shortcuts |= ModMask::ALT;
            }
            if modifiers.logo {
                expected_shortcuts |= ModMask::SUPER;
            }
            assert_eq!(
                mapped.shortcuts, expected_shortcuts,
                "shortcut modifier mismatch for combination {bits:04b}"
            );
        }
    }

    #[test]
    fn keyboard_modifier_adapter_ignores_lock_and_level_modifiers() {
        let mapped = keyboard_modifiers(&ModifiersState {
            caps_lock: true,
            num_lock: true,
            iso_level3_shift: true,
            iso_level5_shift: true,
            ..Default::default()
        });

        assert_eq!(mapped.input, Modifiers::default());
        assert_eq!(mapped.shortcuts, ModMask::empty());
    }

    #[test]
    fn locked_input_routes_only_to_lock_screen_without_activity_recording() {
        assert_eq!(
            input_route(true),
            InputRoute {
                target: InputTarget::LockScreen,
                record_user_activity: false,
            }
        );
    }

    #[test]
    fn unlocked_input_routes_to_desktop_and_records_activity() {
        assert_eq!(
            input_route(false),
            InputRoute {
                target: InputTarget::Desktop,
                record_user_activity: true,
            }
        );
    }

    #[test]
    fn compositor_grab_suppresses_buttons_until_left_release() {
        assert_eq!(
            client_button_delivery(true, MouseButton::Left, KeyState::Pressed),
            ClientButtonDelivery::Suppress
        );
        assert_eq!(
            client_button_delivery(true, MouseButton::Right, KeyState::Released),
            ClientButtonDelivery::Suppress
        );
        assert_eq!(
            client_button_delivery(true, MouseButton::Left, KeyState::Released),
            ClientButtonDelivery::FinishGrab
        );
    }

    #[test]
    fn ungrabbed_buttons_are_forwarded() {
        assert_eq!(
            client_button_delivery(false, MouseButton::Right, KeyState::Pressed),
            ClientButtonDelivery::Forward
        );
    }

    #[test]
    fn desktop_screenshot_shortcut_triggers_once_per_key_cycle() {
        let keybinds = Keybinds::with_defaults(ShortcutCapabilities::DESKTOP);
        let dispatches = [KeyState::Pressed, KeyState::Released].map(|state| {
            shortcut_dispatch(
                &keybinds,
                KeySymbol::from_raw(keysyms::KEY_Print),
                ModMask::empty(),
                state,
            )
        });

        assert_eq!(
            dispatches,
            [
                ShortcutDispatch::Trigger(KeyAction::TakeScreenshot),
                ShortcutDispatch::Intercept,
            ]
        );
        assert_eq!(
            dispatches
                .iter()
                .filter(|dispatch| matches!(dispatch, ShortcutDispatch::Trigger(_)))
                .count(),
            1
        );
    }

    #[test]
    fn nested_screenshot_key_is_forwarded_to_clients() {
        let keybinds = Keybinds::with_defaults(ShortcutCapabilities::NESTED);

        for state in [KeyState::Pressed, KeyState::Released] {
            assert_eq!(
                shortcut_dispatch(
                    &keybinds,
                    KeySymbol::from_raw(keysyms::KEY_Print),
                    ModMask::empty(),
                    state,
                ),
                ShortcutDispatch::Forward
            );
        }
    }

    #[test]
    fn shifted_desktop_screenshot_uses_all_outputs_action_once() {
        let keybinds = Keybinds::with_defaults(ShortcutCapabilities::DESKTOP);

        assert_eq!(
            shortcut_dispatch(
                &keybinds,
                KeySymbol::from_raw(keysyms::KEY_Print),
                ModMask::SHIFT,
                KeyState::Pressed,
            ),
            ShortcutDispatch::Trigger(KeyAction::TakeScreenshotAll)
        );
        assert_eq!(
            shortcut_dispatch(
                &keybinds,
                KeySymbol::from_raw(keysyms::KEY_Print),
                ModMask::SHIFT,
                KeyState::Released,
            ),
            ShortcutDispatch::Intercept
        );
    }
}

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use focaldesk_input::KeyState;
use focaldesk_settings_core::KeyboardAccessibilitySettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyTransition {
    pub keycode: u32,
    pub state: KeyState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRole {
    Shift,
    Modifier,
    Other,
}

impl KeyRole {
    fn is_modifier(self) -> bool {
        !matches!(self, Self::Other)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feedback {
    StickyShortcutRequested,
    ModifierLatched,
    ModifierReleased,
}

#[derive(Debug, Default)]
pub(crate) struct FilterOutput {
    pub events: Vec<KeyTransition>,
    pub feedback: Vec<Feedback>,
}

#[derive(Debug, Clone, Copy)]
struct PendingPress {
    accepted_at: Instant,
    role: KeyRole,
}

#[derive(Debug)]
pub(crate) struct KeyboardAccessibilityFilter {
    config: KeyboardAccessibilitySettings,
    pending: HashMap<u32, PendingPress>,
    delivered_down: HashMap<u32, KeyRole>,
    bounce_rejected_down: HashSet<u32>,
    last_release: HashMap<u32, Instant>,
    latched_modifiers: HashSet<u32>,
    ignore_modifier_release: HashSet<u32>,
    consume_latches_after: Option<u32>,
    shift_taps: u8,
    last_shift_press: Option<Instant>,
}

impl KeyboardAccessibilityFilter {
    pub(crate) fn new(config: KeyboardAccessibilitySettings) -> Self {
        Self {
            config: config.sanitized(),
            pending: HashMap::new(),
            delivered_down: HashMap::new(),
            bounce_rejected_down: HashSet::new(),
            last_release: HashMap::new(),
            latched_modifiers: HashSet::new(),
            ignore_modifier_release: HashSet::new(),
            consume_latches_after: None,
            shift_taps: 0,
            last_shift_press: None,
        }
    }

    pub(crate) fn config(&self) -> KeyboardAccessibilitySettings {
        self.config
    }

    /// Replace the live configuration. Any logically-held keys are released
    /// first so disabling a filter can never leave a modifier stuck down.
    pub(crate) fn reconfigure(
        &mut self,
        config: KeyboardAccessibilitySettings,
    ) -> Vec<KeyTransition> {
        let config = config.sanitized();
        if self.config == config {
            return Vec::new();
        }
        let releases = self.release_all();
        self.config = config;
        releases
    }

    pub(crate) fn process(
        &mut self,
        keycode: u32,
        state: KeyState,
        role: KeyRole,
        now: Instant,
    ) -> FilterOutput {
        let mut output = FilterOutput::default();
        self.track_sticky_shortcut(state, role, now, &mut output.feedback);

        match state {
            KeyState::Pressed => {
                if self.config.bounce_keys
                    && self.last_release.get(&keycode).is_some_and(|released_at| {
                        now.saturating_duration_since(*released_at)
                            < Duration::from_millis(u64::from(self.config.bounce_keys_delay_ms))
                    })
                {
                    self.bounce_rejected_down.insert(keycode);
                    return output;
                }

                if self.config.slow_keys {
                    self.pending.entry(keycode).or_insert(PendingPress {
                        accepted_at: now
                            + Duration::from_millis(u64::from(self.config.slow_keys_delay_ms)),
                        role,
                    });
                    return output;
                }

                self.apply_accepted(KeyTransition { keycode, state }, role, now, &mut output);
            }
            KeyState::Released => {
                if self.bounce_rejected_down.remove(&keycode) {
                    return output;
                }
                if self.pending.remove(&keycode).is_some() {
                    return output;
                }
                let role = self.delivered_down.get(&keycode).copied().unwrap_or(role);
                self.apply_accepted(KeyTransition { keycode, state }, role, now, &mut output);
            }
        }
        output
    }

    pub(crate) fn tick(&mut self, now: Instant) -> FilterOutput {
        let mut ready = self
            .pending
            .iter()
            .filter_map(|(&keycode, pending)| {
                (pending.accepted_at <= now).then_some((pending.accepted_at, keycode, pending.role))
            })
            .collect::<Vec<_>>();
        ready.sort_unstable_by_key(|(accepted_at, keycode, _)| (*accepted_at, *keycode));

        let mut output = FilterOutput::default();
        for (_, keycode, role) in ready {
            self.pending.remove(&keycode);
            self.apply_accepted(
                KeyTransition {
                    keycode,
                    state: KeyState::Pressed,
                },
                role,
                now,
                &mut output,
            );
        }
        output
    }

    fn track_sticky_shortcut(
        &mut self,
        state: KeyState,
        role: KeyRole,
        now: Instant,
        feedback: &mut Vec<Feedback>,
    ) {
        if !self.config.sticky_keys_shortcut || !matches!(state, KeyState::Pressed) {
            return;
        }
        if !matches!(role, KeyRole::Shift) {
            self.shift_taps = 0;
            self.last_shift_press = None;
            return;
        }
        let continues = self.last_shift_press.is_some_and(|previous| {
            now.saturating_duration_since(previous) <= Duration::from_millis(700)
        });
        self.shift_taps = if continues {
            self.shift_taps.saturating_add(1)
        } else {
            1
        };
        self.last_shift_press = Some(now);
        if self.shift_taps == 5 {
            self.shift_taps = 0;
            self.last_shift_press = None;
            feedback.push(Feedback::StickyShortcutRequested);
        }
    }

    fn apply_accepted(
        &mut self,
        transition: KeyTransition,
        role: KeyRole,
        now: Instant,
        output: &mut FilterOutput,
    ) {
        match transition.state {
            KeyState::Pressed => {
                if self.config.sticky_keys && role.is_modifier() {
                    if self.latched_modifiers.remove(&transition.keycode) {
                        output.events.push(KeyTransition {
                            state: KeyState::Released,
                            ..transition
                        });
                        self.ignore_modifier_release.insert(transition.keycode);
                        self.delivered_down.remove(&transition.keycode);
                        output.feedback.push(Feedback::ModifierReleased);
                        return;
                    }
                }
                self.delivered_down.insert(transition.keycode, role);
                output.events.push(transition);
                if self.config.sticky_keys
                    && !role.is_modifier()
                    && !self.latched_modifiers.is_empty()
                {
                    self.consume_latches_after = Some(transition.keycode);
                }
            }
            KeyState::Released => {
                if self.ignore_modifier_release.remove(&transition.keycode) {
                    return;
                }
                self.delivered_down.remove(&transition.keycode);
                self.last_release.insert(transition.keycode, now);
                if self.config.sticky_keys && role.is_modifier() {
                    self.latched_modifiers.insert(transition.keycode);
                    output.feedback.push(Feedback::ModifierLatched);
                    return;
                }
                output.events.push(transition);
                if self.consume_latches_after == Some(transition.keycode) {
                    self.consume_latches_after = None;
                    let mut modifiers = self.latched_modifiers.drain().collect::<Vec<_>>();
                    modifiers.sort_unstable();
                    for keycode in modifiers {
                        self.delivered_down.remove(&keycode);
                        self.last_release.insert(keycode, now);
                        output.events.push(KeyTransition {
                            keycode,
                            state: KeyState::Released,
                        });
                    }
                    output.feedback.push(Feedback::ModifierReleased);
                }
            }
        }
    }

    fn release_all(&mut self) -> Vec<KeyTransition> {
        self.pending.clear();
        self.bounce_rejected_down.clear();
        self.ignore_modifier_release.clear();
        self.latched_modifiers.clear();
        self.consume_latches_after = None;
        self.shift_taps = 0;
        self.last_shift_press = None;
        self.last_release.clear();
        let mut keycodes = self.delivered_down.keys().copied().collect::<Vec<_>>();
        keycodes.sort_unstable();
        self.delivered_down.clear();
        keycodes
            .into_iter()
            .map(|keycode| KeyTransition {
                keycode,
                state: KeyState::Released,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> KeyboardAccessibilitySettings {
        KeyboardAccessibilitySettings::default()
    }

    #[test]
    fn slow_keys_accept_only_after_deadline() {
        let mut config = config();
        config.slow_keys = true;
        config.slow_keys_delay_ms = 500;
        let mut filter = KeyboardAccessibilityFilter::new(config);
        let start = Instant::now();
        assert!(
            filter
                .process(30, KeyState::Pressed, KeyRole::Other, start)
                .events
                .is_empty()
        );
        assert!(
            filter
                .tick(start + Duration::from_millis(499))
                .events
                .is_empty()
        );
        assert_eq!(
            filter.tick(start + Duration::from_millis(500)).events,
            vec![KeyTransition {
                keycode: 30,
                state: KeyState::Pressed
            }]
        );
    }

    #[test]
    fn slow_keys_reject_short_press() {
        let mut config = config();
        config.slow_keys = true;
        let mut filter = KeyboardAccessibilityFilter::new(config);
        let start = Instant::now();
        filter.process(30, KeyState::Pressed, KeyRole::Other, start);
        assert!(
            filter
                .process(
                    30,
                    KeyState::Released,
                    KeyRole::Other,
                    start + Duration::from_millis(50)
                )
                .events
                .is_empty()
        );
        assert!(
            filter
                .tick(start + Duration::from_secs(1))
                .events
                .is_empty()
        );
    }

    #[test]
    fn bounce_keys_reject_repeat_inside_interval() {
        let mut config = config();
        config.bounce_keys = true;
        config.bounce_keys_delay_ms = 300;
        let mut filter = KeyboardAccessibilityFilter::new(config);
        let start = Instant::now();
        assert_eq!(
            filter
                .process(30, KeyState::Pressed, KeyRole::Other, start)
                .events
                .len(),
            1
        );
        assert_eq!(
            filter
                .process(30, KeyState::Released, KeyRole::Other, start)
                .events
                .len(),
            1
        );
        assert!(
            filter
                .process(
                    30,
                    KeyState::Pressed,
                    KeyRole::Other,
                    start + Duration::from_millis(299)
                )
                .events
                .is_empty()
        );
        assert!(
            filter
                .process(
                    30,
                    KeyState::Released,
                    KeyRole::Other,
                    start + Duration::from_millis(300)
                )
                .events
                .is_empty()
        );
    }

    #[test]
    fn sticky_modifier_wraps_next_key() {
        let mut config = config();
        config.sticky_keys = true;
        let mut filter = KeyboardAccessibilityFilter::new(config);
        let now = Instant::now();
        assert_eq!(
            filter
                .process(42, KeyState::Pressed, KeyRole::Shift, now)
                .events
                .len(),
            1
        );
        assert!(
            filter
                .process(42, KeyState::Released, KeyRole::Shift, now)
                .events
                .is_empty()
        );
        assert_eq!(
            filter
                .process(30, KeyState::Pressed, KeyRole::Other, now)
                .events
                .len(),
            1
        );
        assert_eq!(
            filter
                .process(30, KeyState::Released, KeyRole::Other, now)
                .events,
            vec![
                KeyTransition {
                    keycode: 30,
                    state: KeyState::Released
                },
                KeyTransition {
                    keycode: 42,
                    state: KeyState::Released
                },
            ]
        );
    }

    #[test]
    fn fifth_shift_press_requests_confirmation() {
        let mut filter = KeyboardAccessibilityFilter::new(config());
        let start = Instant::now();
        for index in 0..4 {
            let output = filter.process(
                42,
                KeyState::Pressed,
                KeyRole::Shift,
                start + Duration::from_millis(index * 100),
            );
            assert!(output.feedback.is_empty());
            filter.process(
                42,
                KeyState::Released,
                KeyRole::Shift,
                start + Duration::from_millis(index * 100 + 20),
            );
        }
        assert!(
            filter
                .process(
                    42,
                    KeyState::Pressed,
                    KeyRole::Shift,
                    start + Duration::from_millis(400)
                )
                .feedback
                .contains(&Feedback::StickyShortcutRequested)
        );
    }

    #[test]
    fn reconfigure_releases_held_keys_but_an_unchanged_reload_does_not() {
        let mut filter = KeyboardAccessibilityFilter::new(config());
        let now = Instant::now();
        filter.process(30, KeyState::Pressed, KeyRole::Other, now);
        assert!(filter.reconfigure(config()).is_empty());

        let mut changed = config();
        changed.bounce_keys = true;
        assert_eq!(
            filter.reconfigure(changed),
            vec![KeyTransition {
                keycode: 30,
                state: KeyState::Released,
            }]
        );
    }
}

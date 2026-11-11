use super::*;

impl DesktopState {
    pub(crate) fn refresh_magnifier_targets(&mut self) {
        let targets = self
            .outputs
            .iter()
            .map(|(output_id, output)| {
                let zoom = if self.accessibility_settings.magnifier_enabled {
                    self.accessibility_settings
                        .magnifier_output_zoom
                        .get(&output.handle.name())
                        .copied()
                        .unwrap_or(self.accessibility_settings.magnifier_zoom)
                } else {
                    1.0
                };
                (*output_id, f64::from(zoom.clamp(1.0, 8.0)))
            })
            .collect::<Vec<_>>();
        let live = targets
            .iter()
            .map(|(output_id, _)| *output_id)
            .collect::<HashSet<_>>();
        self.magnifier_views
            .retain(|output_id, _| live.contains(output_id));
        for (output_id, zoom) in targets {
            self.magnifier_views
                .entry(output_id)
                .or_default()
                .set_target_zoom(zoom);
        }
    }

    pub(crate) fn tick_magnifier(&mut self) {
        self.refresh_magnifier_targets();
        let animations = self.animations_enabled;
        let changed = self
            .magnifier_views
            .iter_mut()
            .filter_map(|(output_id, view)| view.tick(animations).then_some(*output_id))
            .collect::<Vec<_>>();
        for output_id in changed {
            self.mark_output_full_damage(output_id, DamageSource::Unknown);
        }
    }

    pub(crate) fn magnifier_view_for_output(
        &self,
        output_id: OutputId,
    ) -> crate::core::magnifier::MagnifierView {
        self.magnifier_views
            .get(&output_id)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn magnifier_source_rect(
        &self,
        output_id: OutputId,
        size: Size<i32, Physical>,
    ) -> Rectangle<f64, Buffer> {
        self.magnifier_view_for_output(output_id).source_rect(size)
    }

    pub(crate) fn update_magnifier_pointer_focus(&mut self, position: Point<f64, Logical>) {
        let Some(output_id) = self.output_under_pointer(position) else {
            return;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return;
        };
        let focus = [
            ((position.x - f64::from(output.logical_origin.x))
                / f64::from(output.logical_size.w.max(1)))
            .clamp(0.0, 1.0),
            ((position.y - f64::from(output.logical_origin.y))
                / f64::from(output.logical_size.h.max(1)))
            .clamp(0.0, 1.0),
        ];
        self.magnifier_views
            .entry(output_id)
            .or_default()
            .set_target_focus(focus);
    }

    pub(crate) fn update_magnifier_keyboard_focus(&mut self) {
        if !self.accessibility_settings.magnifier_follow_focus {
            return;
        }
        let Some(window_id) = self.focused_window else {
            return;
        };
        let Some(managed) = self.window(window_id) else {
            return;
        };
        let output_id = managed.output.unwrap_or(self.focused_output);
        let Some(rect) = self.global_window_bbox(&managed.window) else {
            return;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return;
        };
        let center_x = f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0;
        let center_y = f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0;
        let focus = [
            ((center_x - f64::from(output.logical_origin.x))
                / f64::from(output.logical_size.w.max(1)))
            .clamp(0.0, 1.0),
            ((center_y - f64::from(output.logical_origin.y))
                / f64::from(output.logical_size.h.max(1)))
            .clamp(0.0, 1.0),
        ];
        self.magnifier_views
            .entry(output_id)
            .or_default()
            .set_target_focus(focus);
    }

    pub(crate) fn adjust_magnifier_zoom(&mut self, steps: i32) {
        let output_id = self.focused_output;
        let current = self
            .magnifier_views
            .get(&output_id)
            .map(|view| view.target_zoom)
            .unwrap_or(1.0);
        let next = (current + f64::from(steps) * 0.25).clamp(1.0, 8.0);
        if (next - current).abs() < f64::EPSILON {
            return;
        }
        let Some(connector) = self
            .outputs
            .get(&output_id)
            .map(|output| output.handle.name())
        else {
            return;
        };

        let mut settings = load_settings();
        settings.accessibility.magnifier_enabled = true;
        settings
            .accessibility
            .magnifier_output_zoom
            .insert(connector, next as f32);
        if let Err(error) = save_settings(&settings) {
            flog_warn!("could not save magnifier setting: {error}");
            return;
        }
        self.accessibility_settings = settings.accessibility;
        self.refresh_magnifier_targets();
        self.mark_output_full_damage(output_id, DamageSource::Unknown);
        notification_service_notify(
            "Screen magnifier",
            format!("{}% on this display", (next * 100.0).round() as u32),
            Some(Duration::from_secs(2)),
        );
    }

    pub(crate) fn cursor_base_size(&self) -> u32 {
        self.accessibility_settings.cursor_size.clamp(24, 96)
    }
}

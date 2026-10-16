use smithay::utils::{Buffer, Physical, Rectangle, Size};

const EPSILON: f64 = 0.001;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MagnifierView {
    pub zoom: f64,
    pub target_zoom: f64,
    /// Normalized output coordinates, kept in range even when modes change.
    pub focus: [f64; 2],
    pub target_focus: [f64; 2],
}

impl Default for MagnifierView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            target_zoom: 1.0,
            focus: [0.5, 0.5],
            target_focus: [0.5, 0.5],
        }
    }
}

impl MagnifierView {
    pub(crate) fn set_target_zoom(&mut self, zoom: f64) {
        self.target_zoom = zoom.clamp(1.0, 8.0);
    }

    pub(crate) fn set_target_focus(&mut self, focus: [f64; 2]) {
        self.target_focus = [focus[0].clamp(0.0, 1.0), focus[1].clamp(0.0, 1.0)];
    }

    /// Move one frame toward the requested viewport. Returns true while a
    /// repaint is needed. Reduced motion snaps directly to the destination.
    pub(crate) fn tick(&mut self, animations: bool) -> bool {
        let old = *self;
        if animations {
            self.zoom = approach(self.zoom, self.target_zoom, 0.24);
            self.focus[0] = approach(self.focus[0], self.target_focus[0], 0.28);
            self.focus[1] = approach(self.focus[1], self.target_focus[1], 0.28);
        } else {
            self.zoom = self.target_zoom;
            self.focus = self.target_focus;
        }
        changed(old.zoom, self.zoom)
            || changed(old.focus[0], self.focus[0])
            || changed(old.focus[1], self.focus[1])
    }

    pub(crate) fn is_animating(&self) -> bool {
        changed(self.zoom, self.target_zoom)
            || changed(self.focus[0], self.target_focus[0])
            || changed(self.focus[1], self.target_focus[1])
    }

    pub(crate) fn source_rect(&self, size: Size<i32, Physical>) -> Rectangle<f64, Buffer> {
        source_rect(size, self.zoom, self.focus)
    }

    pub(crate) fn transform_rect(&self, rect: [i32; 4], size: Size<i32, Physical>) -> [i32; 4] {
        transform_rect(rect, size, self.zoom, self.focus)
    }

    pub(crate) fn transform_point(&self, point: [f32; 2], size: Size<i32, Physical>) -> [f32; 2] {
        // Focus continues to track the pointer while accessibility magnifier
        // is idle, so that enabling it can immediately center the view.  That
        // focus is not a display transform on its own: applying it at 1x
        // moves the pointer toward the output centre as focus catches up.
        if self.zoom <= 1.001 {
            return point;
        }
        let focus_x = self.focus[0] * f64::from(size.w);
        let focus_y = self.focus[1] * f64::from(size.h);
        [
            ((f64::from(point[0]) - focus_x) * self.zoom + f64::from(size.w) / 2.0) as f32,
            ((f64::from(point[1]) - focus_y) * self.zoom + f64::from(size.h) / 2.0) as f32,
        ]
    }
}

fn approach(current: f64, target: f64, fraction: f64) -> f64 {
    let next = current + (target - current) * fraction;
    if changed(next, target) {
        next
    } else {
        target
    }
}

fn changed(left: f64, right: f64) -> bool {
    (left - right).abs() > EPSILON
}

pub(crate) fn source_rect(
    size: Size<i32, Physical>,
    zoom: f64,
    focus: [f64; 2],
) -> Rectangle<f64, Buffer> {
    let zoom = zoom.clamp(1.0, 8.0);
    let source_w = f64::from(size.w) / zoom;
    let source_h = f64::from(size.h) / zoom;
    let max_x = f64::from(size.w) - source_w;
    let max_y = f64::from(size.h) - source_h;
    let center_x = focus[0].clamp(0.0, 1.0) * f64::from(size.w);
    let center_y = focus[1].clamp(0.0, 1.0) * f64::from(size.h);
    Rectangle::from_loc_and_size(
        (
            (center_x - source_w / 2.0).clamp(0.0, max_x),
            (center_y - source_h / 2.0).clamp(0.0, max_y),
        ),
        (source_w, source_h),
    )
}

pub(crate) fn transform_rect(
    rect: [i32; 4],
    size: Size<i32, Physical>,
    zoom: f64,
    focus: [f64; 2],
) -> [i32; 4] {
    let focus_x = focus[0].clamp(0.0, 1.0) * f64::from(size.w);
    let focus_y = focus[1].clamp(0.0, 1.0) * f64::from(size.h);
    [
        ((f64::from(rect[0]) - focus_x) * zoom + f64::from(size.w) / 2.0).round() as i32,
        ((f64::from(rect[1]) - focus_y) * zoom + f64::from(size.h) / 2.0).round() as i32,
        (f64::from(rect[2]) * zoom).round().max(1.0) as i32,
        (f64::from(rect[3]) * zoom).round().max(1.0) as i32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_rect_centers_and_clamps_to_output() {
        let size = Size::from((1_920, 1_080));
        assert_eq!(
            source_rect(size, 2.0, [0.5, 0.5]),
            Rectangle::from_loc_and_size((480.0, 270.0), (960.0, 540.0))
        );
        assert_eq!(
            source_rect(size, 2.0, [0.0, 0.0]),
            Rectangle::from_loc_and_size((0.0, 0.0), (960.0, 540.0))
        );
        assert_eq!(
            source_rect(size, 2.0, [1.0, 1.0]),
            Rectangle::from_loc_and_size((960.0, 540.0), (960.0, 540.0))
        );
    }

    #[test]
    fn scene_transform_places_focus_at_output_center() {
        let size = Size::from((1_000, 800));
        assert_eq!(
            transform_rect([200, 100, 50, 40], size, 2.0, [0.2, 0.125]),
            [500, 400, 100, 80]
        );
    }

    #[test]
    fn idle_magnifier_never_moves_the_cursor_toward_center() {
        let view = MagnifierView {
            zoom: 1.0,
            target_zoom: 1.0,
            // This is the normal follow-pointer state after a pointer move.
            focus: [0.12, 0.84],
            target_focus: [0.12, 0.84],
        };

        assert_eq!(
            view.transform_point([173.0, 619.0], Size::from((1_920, 1_080))),
            [173.0, 619.0]
        );
    }

    #[test]
    fn reduced_motion_snaps_while_animation_converges() {
        let mut view = MagnifierView::default();
        view.set_target_zoom(4.0);
        view.set_target_focus([0.8, 0.2]);
        assert!(view.tick(true));
        assert!(view.zoom > 1.0 && view.zoom < 4.0);
        assert!(view.is_animating());
        assert!(view.tick(false));
        assert_eq!(view.zoom, 4.0);
        assert_eq!(view.focus, [0.8, 0.2]);
        assert!(!view.is_animating());
    }
}

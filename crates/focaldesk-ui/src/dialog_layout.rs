// crates/focaldesk-ui/src/dialog_layout.rs

use crate::dialog::Dialog;
use smithay::utils::{Logical, Rectangle};

pub struct DialogLayout {
    pub screen_rect: Rectangle<i32, Logical>,
    pub bounds: Rectangle<i32, Logical>,
    pub title_rect: Rectangle<i32, Logical>,
    pub message_rect: Rectangle<i32, Logical>,
    pub button_rects: Vec<(usize, Rectangle<i32, Logical>)>,
}

fn clamp_rect_to_bounds(
    mut geometry: Rectangle<i32, Logical>,
    bounds: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    if bounds.size.w <= 0 || bounds.size.h <= 0 {
        return geometry;
    }

    geometry.size.w = geometry.size.w.clamp(1, bounds.size.w);
    geometry.size.h = geometry.size.h.clamp(1, bounds.size.h);

    let max_x = (bounds.loc.x + bounds.size.w - geometry.size.w).max(bounds.loc.x);
    let max_y = (bounds.loc.y + bounds.size.h - geometry.size.h).max(bounds.loc.y);

    geometry.loc.x = geometry.loc.x.clamp(bounds.loc.x, max_x);
    geometry.loc.y = geometry.loc.y.clamp(bounds.loc.y, max_y);
    geometry
}

pub fn layout_dialog(dialog: &Dialog, screen: Rectangle<i32, Logical>) -> DialogLayout {
    let w = 520.min(screen.size.w.max(1));
    let h = 220.min(screen.size.h.max(1));
    let button_count = dialog.buttons.len().max(1);

    let x = screen.loc.x + (screen.size.w - w) / 2;
    let y = screen.loc.y + (screen.size.h - h) / 2;

    let bounds = Rectangle::from_loc_and_size((x, y), (w, h));
    let button_gap = 12.min((w / 8).max(4));
    let available_button_w = (w - 48 - (button_count.saturating_sub(1) as i32) * button_gap).max(1);
    let button_w = (available_button_w / button_count as i32).clamp(1, 104);
    let button_h = 36.min((h - 72).max(1));
    let total_button_w =
        button_count as i32 * button_w + (button_count.saturating_sub(1) as i32) * button_gap;
    let start_x = x + w - 24 - total_button_w;
    let button_y = y + h - 56;
    let button_rects = (0..button_count)
        .map(|idx| {
            let rect = Rectangle::from_loc_and_size(
                (start_x + idx as i32 * (button_w + button_gap), button_y),
                (button_w, button_h),
            );
            (idx, rect)
        })
        .collect();

    DialogLayout {
        screen_rect: screen,
        bounds,
        title_rect: clamp_rect_to_bounds(
            Rectangle::from_loc_and_size((x + 24, y + 20), ((w - 48).max(1), 32)),
            bounds,
        ),
        message_rect: clamp_rect_to_bounds(
            Rectangle::from_loc_and_size((x + 24, y + 64), ((w - 48).max(1), 80)),
            bounds,
        ),
        button_rects,
    }
}

#[cfg(test)]
mod tests {
    use super::layout_dialog;
    use crate::dialog::{Dialog, DialogButton, DialogKind, DialogState};
    use focaldesk_types::OutputId;
    use smithay::utils::Rectangle;

    #[test]
    fn layout_dialog_keeps_bounds_and_controls_inside_small_screen() {
        let dialog = Dialog {
            id: 1,
            kind: DialogKind::Confirm,
            title: "Title".into(),
            message: "Message".into(),
            buttons: vec![
                DialogButton {
                    label: "Cancel".into(),
                    action: crate::dialog::DialogAction::Cancel,
                },
                DialogButton {
                    label: "Confirm".into(),
                    action: crate::dialog::DialogAction::Confirm,
                },
            ],
            modal: true,
            dismissible: true,
            state: DialogState::Open,
            owner_output: OutputId(1),
            bounds: Rectangle::from_loc_and_size((0, 0), (1, 1)),
        };

        let layout = layout_dialog(&dialog, Rectangle::from_loc_and_size((0, 0), (300, 120)));

        assert!(layout.bounds.loc.x >= 0);
        assert!(layout.bounds.loc.y >= 0);
        assert!(layout.bounds.loc.x + layout.bounds.size.w <= 300);
        assert!(layout.bounds.loc.y + layout.bounds.size.h <= 120);
        assert!(layout.button_rects.iter().all(|(_, rect)| rect.loc.x >= 0
            && rect.loc.y >= 0
            && rect.loc.x + rect.size.w <= 300
            && rect.loc.y + rect.size.h <= 120));
    }
}

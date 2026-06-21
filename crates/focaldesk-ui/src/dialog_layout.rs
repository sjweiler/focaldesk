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

pub fn layout_dialog(dialog: &Dialog, screen: Rectangle<i32, Logical>) -> DialogLayout {
    let w = 520;
    let h = 220;
    let button_count = dialog.buttons.len().max(1);

    let x = screen.loc.x + (screen.size.w - w) / 2;
    let y = screen.loc.y + (screen.size.h - h) / 2;

    let bounds = Rectangle::from_loc_and_size((x, y), (w, h));
    let button_gap = 12;
    let button_w = 104;
    let button_h = 36;
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
        title_rect: Rectangle::from_loc_and_size((x + 24, y + 20), (w - 48, 32)),
        message_rect: Rectangle::from_loc_and_size((x + 24, y + 64), (w - 48, 80)),
        button_rects,
    }
}

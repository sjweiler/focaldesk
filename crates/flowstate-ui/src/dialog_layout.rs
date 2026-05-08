// crates/flowstate-ui/src/dialog_layout.rs

use smithay::utils::{Logical, Rectangle};
use crate::dialog::Dialog;

pub struct DialogLayout {
    pub screen_rect: Rectangle<i32, Logical>,
    pub bounds: Rectangle<i32, Logical>,
    pub title_rect: Rectangle<i32, Logical>,
    pub message_rect: Rectangle<i32, Logical>,
    pub button_rects: Vec<(usize, Rectangle<i32, Logical>)>,
}

pub fn layout_dialog(_dialog: &Dialog, screen: Rectangle<i32, Logical>) -> DialogLayout {
    let w = 520;
    let h = 220;

    let x = screen.loc.x + (screen.size.w - w) / 2;
    let y = screen.loc.y + (screen.size.h - h) / 2;

    let bounds = Rectangle::from_loc_and_size((x, y), (w, h));

    DialogLayout {
        screen_rect: screen,
        bounds,
        title_rect: Rectangle::from_loc_and_size((x + 24, y + 20), (w - 48, 32)),
        message_rect: Rectangle::from_loc_and_size((x + 24, y + 64), (w - 48, 80)),
        button_rects: vec![
            (0, Rectangle::from_loc_and_size((x + w - 220, y + h - 56), (90, 36))),
            (1, Rectangle::from_loc_and_size((x + w - 116, y + h - 56), (90, 36))),
        ],
    }
}

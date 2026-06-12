pub struct DialogLayout {
    pub bounds: Rect<i32, Logical>,
    pub title_rect: Rect<i32, Logical>,
    pub message_rect: Rect<i32, Logical>,
    pub button_rects: Vec<(usize, Rect<i32, Logical>)>,
}

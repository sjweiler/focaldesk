use std::time::Instant;

pub struct ChromeInteraction {
    pub hovered: Option<usize>,
    pub pressed: Option<usize>,
}

pub struct TooltipState {
    pub hovered: Option<usize>,
    pub start: Option<Instant>,
    pub visible: bool,
}

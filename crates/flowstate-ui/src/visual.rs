#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiVisualState {
    Inactive,
    Hover,
    Active,
    Selected,
    Disabled,
}

#[derive(Debug, Clone, Copy)]
pub struct UiVisualStyle {
    pub tint: [f32; 4],
    pub glow: f32,
    pub alpha: f32,
    pub scale: f32,
}

pub fn visual_style(state: UiVisualState) -> UiVisualStyle {
    match state {
        UiVisualState::Inactive => UiVisualStyle {
            tint: [0.45, 0.65, 0.95, 0.70],
            glow: 0.0,
            alpha: 0.70,
            scale: 1.0,
        },
        UiVisualState::Hover => UiVisualStyle {
            tint: [0.75, 0.90, 1.0, 0.95],
            glow: 0.12,
            alpha: 0.95,
            scale: 1.04,
        },
        UiVisualState::Active => UiVisualStyle {
            tint: [1.0, 0.72, 0.22, 1.0],
            glow: 0.25,
            alpha: 1.0,
            scale: 1.0,
        },
        UiVisualState::Selected => UiVisualStyle {
            tint: [0.35, 0.85, 1.0, 1.0],
            glow: 0.18,
            alpha: 1.0,
            scale: 1.02,
        },
        UiVisualState::Disabled => UiVisualStyle {
            tint: [0.25, 0.30, 0.38, 0.35],
            glow: 0.0,
            alpha: 0.35,
            scale: 1.0,
        },
    }
}



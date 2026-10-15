use std::ops::Add;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl From<(f64, f64)> for Position {
    fn from((x, y): (f64, f64)) -> Self {
        Self::new(x, y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Delta {
    pub x: f64,
    pub y: f64,
}

impl Delta {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl From<(f64, f64)> for Delta {
    fn from((x, y): (f64, f64)) -> Self {
        Self::new(x, y)
    }
}

impl Add<Delta> for Position {
    type Output = Self;

    fn add(self, delta: Delta) -> Self::Output {
        Self::new(self.x + delta.x, self.y + delta.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

impl From<(f64, f64)> for Size {
    fn from((width, height): (f64, f64)) -> Self {
        Self::new(width, height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bounds {
    pub origin: Position,
    pub size: Size,
}

impl Bounds {
    pub const fn new(origin: Position, size: Size) -> Self {
        Self { origin, size }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

impl OutputTransform {
    pub const fn inverse(self) -> Self {
        match self {
            Self::Normal => Self::Normal,
            Self::Rotate90 => Self::Rotate270,
            Self::Rotate180 => Self::Rotate180,
            Self::Rotate270 => Self::Rotate90,
            Self::Flipped => Self::Flipped,
            Self::Flipped90 => Self::Flipped270,
            Self::Flipped180 => Self::Flipped180,
            Self::Flipped270 => Self::Flipped90,
        }
    }

    pub const fn transform_size(self, size: Size) -> Size {
        match self {
            Self::Rotate90 | Self::Rotate270 | Self::Flipped90 | Self::Flipped270 => {
                Size::new(size.height, size.width)
            }
            _ => size,
        }
    }
}

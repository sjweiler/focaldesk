use crate::{OutputTransform, Position, Size};

/// Maps absolute-device coordinates through the orientation of an output.
pub fn transform_absolute_position(
    position: Position,
    source_size: Size,
    transform: OutputTransform,
) -> Position {
    match transform {
        OutputTransform::Normal => position,
        OutputTransform::Rotate90 => Position::new(source_size.height - position.y, position.x),
        OutputTransform::Rotate180 => Position::new(
            source_size.width - position.x,
            source_size.height - position.y,
        ),
        OutputTransform::Rotate270 => Position::new(position.y, source_size.width - position.x),
        OutputTransform::Flipped => Position::new(source_size.width - position.x, position.y),
        OutputTransform::Flipped90 => Position::new(position.y, position.x),
        OutputTransform::Flipped180 => Position::new(position.x, source_size.height - position.y),
        OutputTransform::Flipped270 => Position::new(
            source_size.height - position.y,
            source_size.width - position.x,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_pointer_coordinates_follow_output_orientation() {
        let size = Size::new(1920.0, 1080.0);
        let point = Position::new(480.0, 270.0);

        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Normal),
            (480.0, 270.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Rotate90),
            (810.0, 480.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Rotate180),
            (1440.0, 810.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Rotate270),
            (270.0, 1440.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Flipped),
            (1440.0, 270.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Flipped90),
            (270.0, 480.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Flipped180),
            (480.0, 810.0).into()
        );
        assert_eq!(
            transform_absolute_position(point, size, OutputTransform::Flipped270),
            (810.0, 1440.0).into()
        );
    }
}

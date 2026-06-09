use crate::element::UiElement;
use crate::uicomponent::LayoutCtx;
use crate::uicomponent::RenderCtx;
use crate::uicomponent::UiComponent;
use crate::uicomponent::UiHit;
use crate::uicomponent::UiHitTarget;
use flowstate_types::WidgetId;
use smithay::backend::renderer::gles::GlesError;
use smithay::utils::Logical;
use smithay::utils::Point;

#[derive(Debug, Clone)]
pub enum ClockHourFormat {
    Twelve,
    TwentyFour,
}

#[derive(Debug, Clone)]
pub struct ClockComponent {
    pub hour_format: ClockHourFormat,
    pub show_seconds: bool,
    pub show_date: bool,
    pub bounds: crate::element::UiRect,
    pub elements: Vec<UiElement>,
}

impl Default for ClockComponent {
    fn default() -> Self {
        Self {
            hour_format: ClockHourFormat::Twelve,
            show_seconds: false,
            show_date: false,
            bounds: crate::element::UiRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            elements: Vec::new(),
        }
    }
}

impl ClockComponent {
    pub fn format_time(&self, now: chrono::DateTime<chrono::Local>) -> String {
        match self.hour_format {
            ClockHourFormat::Twelve => {
                if self.show_seconds {
                    now.format("%-I:%M:%S %p").to_string()
                } else {
                    now.format("%-I:%M %p").to_string()
                }
            }

            ClockHourFormat::TwentyFour => {
                if self.show_seconds {
                    now.format("%H:%M:%S").to_string()
                } else {
                    now.format("%H:%M").to_string()
                }
            }
        }
    }

    pub fn format_date(&self, now: chrono::DateTime<chrono::Local>) -> Option<String> {
        if self.show_date {
            Some(now.format("%b %-d, %Y").to_string())
        } else {
            None
        }
    }
}

impl UiComponent for ClockComponent {
    fn layout(&mut self, _ctx: &LayoutCtx) {
        // clock layout
    }

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::TopBar,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None
    }

    fn render(&self, _ctx: &mut RenderCtx) -> Result<(), GlesError> {
        // temporary no-op is fine
        Ok(())
    }
}

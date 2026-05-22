use flowstate_themes::FlowTheme;
use smithay::backend::renderer::gles::GlesFrame;
use smithay::utils::{Logical, Physical, Point, Rectangle};

use crate::dialog::{Dialog, DialogId};
use crate::dialog_layout::layout_dialog;
use crate::dialog_render::draw_dialog;
use crate::desktop_frame::DesktopFrameCtx;
use crate::uicomponent::UiHit;
use crate::chrome_shaders::ChromeShaders;

pub struct DialogLayer {
    pub dialogs: Vec<Dialog>,
    pub active_modal: Option<DialogId>,
}

impl DialogLayer {
    pub fn layout(&mut self, screen: Rectangle<i32, Logical>) {
        for dialog in &mut self.dialogs {
            let laid_out = layout_dialog(dialog, screen);
            dialog.bounds = laid_out.bounds;
        }
    }

    pub fn render(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        shaders: &ChromeShaders,
        theme: &FlowTheme,
        active_dialog: Option<DialogId>,
        draw_on_this_output: bool,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        let Some(dialog_id) = active_dialog else {
            return Ok(());
        };
        let Some(dialog) = self.dialogs.iter().find(|d| d.id == dialog_id) else {
            return Ok(());
        };
        let Some(rounded) = shaders.rounded_rect.as_ref() else {
            return Ok(());
        };

        let layout = layout_dialog(dialog, frame_ctx.work);
        draw_dialog(
            frame,
            frame_ctx,
            dialog,
            &layout,
            rounded,
            draw_on_this_output,
            theme,
        )
    }

    pub fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for dialog in self.dialogs.iter().rev() {
            if let Some(hit) = dialog.hit_test(point) {
                return Some(hit);
            }
        }
        None
    }

    pub fn open(&mut self, dialog: Dialog) {
        self.dialogs.push(dialog);
    }

    pub fn close(&mut self, id: DialogId) {
        self.dialogs.retain(|dialog| dialog.id != id);
        if self.active_modal == Some(id) {
            self.active_modal = None;
        }
    }
}

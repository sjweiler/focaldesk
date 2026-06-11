#![allow(unused_imports)]

use flowstate_types::OutputId;
use indexmap::IndexMap;
use smithay::utils::{Physical, Rectangle, Scale}; // your custom OutputId
                                                  //#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
                                                  //pub struct OutputId(pub u32);
use crate::core::FrameCtx;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::utils::Transform;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;
use wayland_server::backend::GlobalId;
use wayland_server::DisplayHandle;

pub struct OutputState {
    pub active_output: OutputId,
    pub outputs: IndexMap<OutputId, OutputCtx>,
}

pub struct OutputCtx {
    pub output: Option<Output>,
    pub physical_size: (i32, i32),
    pub scale: Scale<f64>,
    pub buffer_scale: i32,

    // user-configurable layout
    pub logical_origin: (i32, i32), // desktop space
    pub logical_size: (i32, i32),

    pub enabled: bool,
    pub is_primary: bool,

    // user-configurable ordering override (optional)
    pub ui_order: Option<i32>, // if set, overrides positional sort

    pub last_damage: Vec<Rectangle<i32, Physical>>,

    pub global: Option<GlobalId>,
}

impl OutputState {
    pub fn new_single_nested(size: (i32, i32), scale: f64) -> Self {
        let id = OutputId(1);
        let s = Scale::from(scale);
        let buffer_scale = scale.round().max(1.0) as i32;
        let logical_size = (
            (size.0 as f64 / scale).round() as i32,
            (size.1 as f64 / scale).round() as i32,
        );

        let output = Output::new(
            "flowstate-nested".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "FocusShell".into(),
                model: "Winit".into(),
                serial_number: "nested-0".into(),
            },
        );

        let mode = Mode {
            size: size.into(),
            refresh: 60_000,
        };

        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(smithay::output::Scale::Integer(
                scale.round().max(1.0) as i32
            )),
            Some((0, 0).into()),
        );

        output.set_preferred(mode.clone());

        let mut outputs = IndexMap::new();
        outputs.insert(
            id,
            OutputCtx {
                output: Some(output),
                physical_size: size,
                scale: s,
                buffer_scale,
                logical_origin: (0, 0),
                logical_size,
                enabled: true,
                is_primary: true,
                ui_order: None,
                last_damage: Vec::new(),
                global: None,
            },
        );

        Self {
            active_output: id,
            outputs,
        }
    }
    pub fn ensure_nested_output(
        &mut self,
        output: Output,
        size: (i32, i32),
        scale: f64,
    ) -> OutputId {
        // pick ONE id and stick with it
        let id = self.active_output; // or OutputId(1) if you want to keep that convention
        let id = if id.0 == 0 { OutputId(1) } else { id }; // optional; remove if you prefer OutputId(0)

        let s = Scale::from(scale);
        let buffer_scale = scale.round().max(1.0) as i32;

        let logical_size = (
            (size.0 as f64 / scale).round() as i32,
            (size.1 as f64 / scale).round() as i32,
        );

        match self.outputs.get_mut(&id) {
            Some(ctx) => {
                // update existing
                ctx.output = Some(output);
                ctx.physical_size = size;
                ctx.scale = s;
                ctx.buffer_scale = buffer_scale;
                ctx.logical_origin = (0, 0);
                ctx.logical_size = logical_size;
                ctx.enabled = true;
                ctx.is_primary = true;
                // keep ui_order and last_damage unless you intentionally reset
            }
            None => {
                // insert new (same as new_single)
                self.outputs.insert(
                    id,
                    OutputCtx {
                        output: Some(output),
                        physical_size: size,
                        scale: s,
                        buffer_scale,
                        logical_origin: (0, 0),
                        logical_size,
                        enabled: true,
                        is_primary: true,
                        ui_order: None,
                        last_damage: Vec::new(),
                        global: None,
                    },
                );
            }
        }

        self.active_output = id;
        id
    }

    pub fn full_damage(&self, out_id: OutputId) -> Vec<Rectangle<i32, Physical>> {
        let o = &self.outputs[&out_id];
        vec![Rectangle::from_loc_and_size(
            (0, 0),
            (o.physical_size.0, o.physical_size.1),
        )]
    }
    pub fn new_single(output: Output, size: (i32, i32), scale: f64) -> Self {
        let id = OutputId(1);

        let s = Scale::from(scale);
        let buffer_scale = scale.round().max(1.0) as i32;

        let logical_size = (
            (size.0 as f64 / scale).round() as i32,
            (size.1 as f64 / scale).round() as i32,
        );

        let mut outputs = IndexMap::new();
        outputs.insert(
            id,
            OutputCtx {
                output: Some(output),
                physical_size: size,
                scale: s,
                buffer_scale,
                logical_origin: (0, 0),
                logical_size,
                enabled: true,
                is_primary: true,
                ui_order: None,
                last_damage: Vec::new(),
                global: None,
            },
        );

        Self {
            active_output: id,
            outputs,
        }
    }

    pub fn iter_enabled(&self) -> impl Iterator<Item = (&OutputId, &OutputCtx)> {
        self.outputs.iter().filter(|(_, o)| o.enabled)
    }

    pub fn iter_enabled_mut(&mut self) -> impl Iterator<Item = (&OutputId, &mut OutputCtx)> {
        self.outputs.iter_mut().filter(|(_, o)| o.enabled)
    }
    pub fn build_frame_ctx<'a>(
        &'a self,
        out_id: OutputId,
        damage: &'a [Rectangle<i32, Physical>],
        frame_no: u64,
        now: Instant,
        dt: Duration,
    ) -> FrameCtx {
        let o = &self.outputs[&out_id];

        FrameCtx {
            output_size: o.physical_size,
            output_scale: o.scale,
            buffer_scale: o.buffer_scale,
            damage: damage.to_vec(),
            work: smithay::utils::Rectangle::from_loc_and_size(
                (o.logical_origin.0, o.logical_origin.1),
                (o.logical_size.0, o.logical_size.1),
            ),
            frame_no,
            now,
            dt,
            active_output: self.active_output,
            rendering_output: out_id,
            focus_pulse: 0.0,
            portal_capture: false,
        }
    }
}

impl Default for OutputState {
    fn default() -> Self {
        Self {
            active_output: OutputId(0),
            outputs: IndexMap::new(),
        }
    }
}

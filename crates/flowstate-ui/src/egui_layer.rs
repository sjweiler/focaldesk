//! egui overlay — rendered last, above dialogs and compositor chrome.

use std::{collections::HashMap, mem, ptr};

use egui::{
    epaint::Primitive, ClippedPrimitive, Context, Event, ImageData, Modifiers, MouseWheelUnit,
    PointerButton, Pos2, RawInput, Rect, TextureId, TexturesDelta, Vec2,
};
use flowstate_themes::FlowTheme;
use smithay::backend::renderer::gles::{ffi, GlesError, GlesFrame, GlesPixelProgram, Uniform};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size};

use crate::chrome_shaders::{ChromeShaders, EGUI_FRAG, EGUI_VERT};
use crate::desktop_frame::DesktopFrameCtx;
use crate::types::{PanelKind, UiAction};

pub struct EguiLayer {
    ctx: Context,
    raw_input: RawInput,
    actions: Vec<UiAction>,
    textures_delta: TexturesDelta,
    primitives: Vec<ClippedPrimitive>,
    gl_textures: HashMap<TextureId, EguiGlTexture>,
    gl_program: Option<EguiGlProgram>,
    gl_vbo: u32,
    gl_ibo: u32,
    wants_pointer_input: bool,
    wants_keyboard_input: bool,
    demo_open: bool,
}

#[derive(Debug, Clone, Copy)]
struct EguiGlTexture {
    id: u32,
}

#[derive(Debug, Clone, Copy)]
struct EguiGlProgram {
    id: u32,
    a_pos: i32,
    a_uv: i32,
    a_color: i32,
    u_screen_size: i32,
    u_tex: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EguiInputEvent {
    PointerMoved {
        position: Point<f64, Logical>,
    },
    PointerButton {
        button: EguiPointerButton,
        pressed: bool,
        position: Point<f64, Logical>,
        modifiers: EguiModifiers,
    },
    PointerScroll {
        delta: EguiScrollDelta,
        position: Point<f64, Logical>,
        modifiers: EguiModifiers,
    },
    PointerGone,
    Key {
        key: Option<egui::Key>,
        pressed: bool,
        repeat: bool,
        modifiers: EguiModifiers,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EguiPointerButton {
    Primary,
    Secondary,
    Middle,
    Extra1,
    Extra2,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EguiModifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub mac_cmd: bool,
    pub command: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EguiScrollDelta {
    Line { x: f32, y: f32 },
    Point { x: f32, y: f32 },
}

impl Default for EguiLayer {
    fn default() -> Self {
        Self {
            ctx: Context::default(),
            raw_input: RawInput::default(),
            actions: Vec::new(),
            textures_delta: TexturesDelta::default(),
            primitives: Vec::new(),
            gl_textures: HashMap::new(),
            gl_program: None,
            gl_vbo: 0,
            gl_ibo: 0,
            wants_pointer_input: false,
            wants_keyboard_input: false,
            demo_open: true,
        }
    }
}

/// Gives egui overlay code access to FlowState's compiled shader set and current GLES frame.
///
/// This is intentionally thin: egui can own interaction/layout, while compositor-native effects
/// still render through the same `ChromeShaders` programs used by the shell.
pub struct EguiShaderBridge<'a, 'frame, 'buffer> {
    pub frame: &'a mut GlesFrame<'frame, 'buffer>,
    pub frame_ctx: &'a DesktopFrameCtx,
    pub damage: &'a [Rectangle<i32, Physical>],
    pub shaders: &'a ChromeShaders,
    pub theme: &'a FlowTheme,
}

impl<'a, 'frame, 'buffer> EguiShaderBridge<'a, 'frame, 'buffer> {
    pub fn draw_pixel_shader(
        &mut self,
        program: &GlesPixelProgram,
        rect: Rectangle<i32, Logical>,
        uniforms: &[Uniform<'_>],
    ) -> Result<(), GlesError> {
        let rect_physical: Rectangle<i32, Physical> =
            rect.to_physical_precise_round(self.frame_ctx.output_scale);
        let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );
        let buffer_size =
            Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));

        self.frame.render_pixel_shader_to(
            program,
            src_rect,
            rect_physical,
            buffer_size,
            Some(self.damage),
            1.0,
            uniforms,
        )
    }

    pub fn draw_rounded_rect(
        &mut self,
        rect: Rectangle<i32, Logical>,
        radius: f32,
        color: [f32; 4],
    ) -> Result<(), GlesError> {
        let Some(program) = self.shaders.rounded_rect.as_ref() else {
            return Ok(());
        };

        let rect_physical: Rectangle<i32, Physical> =
            rect.to_physical_precise_round(self.frame_ctx.output_scale);
        self.draw_pixel_shader(
            program,
            rect,
            &[
                Uniform::new("u_size", [rect_physical.size.w as f32, rect_physical.size.h as f32]),
                Uniform::new("u_radius", radius),
                Uniform::new("u_color", color),
            ],
        )
    }
}

impl EguiLayer {
    pub fn handle_input(&mut self, event: EguiInputEvent) -> bool {
        match event {
            EguiInputEvent::PointerMoved { position } => {
                self.raw_input
                    .events
                    .push(Event::PointerMoved(Pos2::new(position.x as f32, position.y as f32)));
            }
            EguiInputEvent::PointerButton {
                button,
                pressed,
                position,
                modifiers,
            } => {
                self.raw_input.events.push(Event::PointerButton {
                    pos: Pos2::new(position.x as f32, position.y as f32),
                    button: pointer_button(button),
                    pressed,
                    modifiers: modifiers.into(),
                });
            }
            EguiInputEvent::PointerScroll {
                delta,
                modifiers,
                ..
            } => {
                let (unit, delta) = match delta {
                    EguiScrollDelta::Line { x, y } => (MouseWheelUnit::Line, Vec2::new(x, y)),
                    EguiScrollDelta::Point { x, y } => (MouseWheelUnit::Point, Vec2::new(x, y)),
                };
                self.raw_input.events.push(Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: modifiers.into(),
                });
            }
            EguiInputEvent::PointerGone => {
                self.raw_input.events.push(Event::PointerGone);
            }
            EguiInputEvent::Key {
                key,
                pressed,
                repeat,
                modifiers,
            } => {
                if let Some(key) = key {
                    self.raw_input.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat,
                        modifiers: modifiers.into(),
                    });
                }
            }
        }

        self.wants_pointer_input || self.wants_keyboard_input
    }

    pub fn wants_pointer_input(&self) -> bool {
        self.wants_pointer_input
    }

    pub fn wants_keyboard_input(&self) -> bool {
        self.wants_keyboard_input
    }

    pub fn take_actions(&mut self) -> Vec<UiAction> {
        mem::take(&mut self.actions)
    }

    pub fn render(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        damage: &[Rectangle<i32, Physical>],
        shaders: &ChromeShaders,
        theme: &FlowTheme,
    ) -> Result<(), GlesError> {
        if frame_ctx.rendering_output != frame_ctx.active_output {
            return Ok(());
        }

        self.run_default_overlay(frame_ctx);

        let mut bridge = EguiShaderBridge {
            frame,
            frame_ctx,
            damage,
            shaders,
            theme,
        };
        self.paint_shader_effects(&mut bridge)?;
        self.render_egui_gl(frame, frame_ctx)
    }

    pub fn render_with_shader_bridge(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        damage: &[Rectangle<i32, Physical>],
        shaders: &ChromeShaders,
        theme: &FlowTheme,
        paint: impl FnOnce(&mut EguiShaderBridge<'_, '_, '_>) -> Result<(), GlesError>,
    ) -> Result<(), GlesError> {
        let mut bridge = EguiShaderBridge {
            frame,
            frame_ctx,
            damage,
            shaders,
            theme,
        };
        paint(&mut bridge)
    }

    pub fn run_ui(
        &mut self,
        frame_ctx: &DesktopFrameCtx,
        build: impl FnOnce(&Context, &mut Vec<UiAction>),
    ) {
        self.prepare_raw_input(frame_ctx);
        self.ctx
            .set_pixels_per_point(frame_ctx.output_scale.x as f32);
        let mut actions = Vec::new();
        let mut build = Some(build);
        let output = self.ctx.run(self.raw_input.take(), |ctx| {
            if let Some(build) = build.take() {
                build(ctx, &mut actions);
            }
        });
        self.textures_delta.append(output.textures_delta);
        self.primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        self.actions.extend(actions);
        self.wants_pointer_input = self.ctx.wants_pointer_input() || self.ctx.is_using_pointer();
        self.wants_keyboard_input = self.ctx.wants_keyboard_input();
    }

    fn run_default_overlay(&mut self, frame_ctx: &DesktopFrameCtx) {
        let demo_open = self.demo_open;
        self.run_ui(frame_ctx, |ctx, actions| {
            if !demo_open {
                return;
            }

            egui::Area::new("flowstate_egui_overlay".into())
                .fixed_pos(egui::pos2(24.0, 96.0))
                .show(ctx, |ui| {
                    ui.set_min_width(220.0);
                    ui.heading("FlowState");
                    ui.separator();
                    if ui.button("Open settings panel").clicked() {
                        actions.push(UiAction::OpenPanel(PanelKind::Settings));
                    }
                    if ui.button("Launch terminal").clicked() {
                        actions.push(UiAction::LaunchApp("weston-terminal"));
                    }
                    if ui.button("Custom action").clicked() {
                        actions.push(UiAction::Custom(9001));
                    }
                });
        });
    }

    fn paint_shader_effects(
        &self,
        bridge: &mut EguiShaderBridge<'_, '_, '_>,
    ) -> Result<(), GlesError> {
        if !self.demo_open {
            return Ok(());
        }

        bridge.draw_rounded_rect(
            Rectangle::from_loc_and_size((18, 88), (244, 184)),
            18.0,
            [0.03, 0.05, 0.075, 0.34],
        )
    }

    fn prepare_raw_input(&mut self, frame_ctx: &DesktopFrameCtx) {
        self.raw_input.screen_rect = Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(
                (frame_ctx.output_size.0 as f64 / frame_ctx.output_scale.x) as f32,
                (frame_ctx.output_size.1 as f64 / frame_ctx.output_scale.y) as f32,
            ),
        ));
        self.raw_input.time = Some(
            frame_ctx
                .now
                .duration_since(frame_ctx.start_time)
                .as_secs_f64(),
        );
        self.raw_input.predicted_dt = 1.0 / 60.0;
    }

    fn render_egui_gl(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
    ) -> Result<(), GlesError> {
        let screen_size = [
            (frame_ctx.output_size.0 as f64 / frame_ctx.output_scale.x) as f32,
            (frame_ctx.output_size.1 as f64 / frame_ctx.output_scale.y) as f32,
        ];
        let output_height = frame_ctx.output_size.1;
        let scale = frame_ctx.output_scale.x as f32;

        frame.with_context(|gl| unsafe {
            self.ensure_gl_resources(gl);
            self.upload_egui_textures(gl);
            self.paint_primitives(gl, screen_size, output_height, scale);
            self.free_egui_textures(gl);
        })
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn ensure_gl_resources(&mut self, gl: &ffi::Gles2) {
        if self.gl_program.is_none() {
            self.gl_program = compile_egui_program(gl);
        }

        if self.gl_vbo == 0 {
            gl.GenBuffers(1, &mut self.gl_vbo);
        }
        if self.gl_ibo == 0 {
            gl.GenBuffers(1, &mut self.gl_ibo);
        }
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn upload_egui_textures(&mut self, gl: &ffi::Gles2) {
        for (id, delta) in self.textures_delta.set.drain(..) {
            let rgba = image_delta_rgba(&delta.image);
            let [w, h] = delta.image.size();

            let texture = self.gl_textures.entry(id).or_insert_with(|| {
                let mut tex = 0;
                unsafe {
                    gl.GenTextures(1, &mut tex);
                }
                EguiGlTexture { id: tex }
            });

            gl.BindTexture(ffi::TEXTURE_2D, texture.id);
            gl.PixelStorei(ffi::UNPACK_ALIGNMENT, 1);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);

            if let Some([x, y]) = delta.pos {
                gl.TexSubImage2D(
                    ffi::TEXTURE_2D,
                    0,
                    x as i32,
                    y as i32,
                    w as i32,
                    h as i32,
                    ffi::RGBA,
                    ffi::UNSIGNED_BYTE,
                    rgba.as_ptr() as *const _,
                );
            } else {
                gl.TexImage2D(
                    ffi::TEXTURE_2D,
                    0,
                    ffi::RGBA as i32,
                    w as i32,
                    h as i32,
                    0,
                    ffi::RGBA,
                    ffi::UNSIGNED_BYTE,
                    rgba.as_ptr() as *const _,
                );
            }
        }
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn free_egui_textures(&mut self, gl: &ffi::Gles2) {
        for id in self.textures_delta.free.drain(..) {
            if let Some(texture) = self.gl_textures.remove(&id) {
                gl.DeleteTextures(1, &texture.id);
            }
        }
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn paint_primitives(
        &mut self,
        gl: &ffi::Gles2,
        screen_size: [f32; 2],
        output_height: i32,
        scale: f32,
    ) {
        let Some(program) = self.gl_program else {
            return;
        };

        gl.UseProgram(program.id);
        gl.Uniform2f(program.u_screen_size, screen_size[0], screen_size[1]);
        gl.Uniform1i(program.u_tex, 0);
        gl.ActiveTexture(ffi::TEXTURE0);

        gl.Enable(ffi::BLEND);
        gl.BlendEquation(ffi::FUNC_ADD);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.Disable(ffi::CULL_FACE);
        gl.Disable(ffi::DEPTH_TEST);
        gl.Enable(ffi::SCISSOR_TEST);

        gl.BindBuffer(ffi::ARRAY_BUFFER, self.gl_vbo);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, self.gl_ibo);

        let stride = (8 * mem::size_of::<f32>()) as i32;
        gl.EnableVertexAttribArray(program.a_pos as u32);
        gl.EnableVertexAttribArray(program.a_uv as u32);
        gl.EnableVertexAttribArray(program.a_color as u32);
        gl.VertexAttribPointer(program.a_pos as u32, 2, ffi::FLOAT, ffi::FALSE, stride, ptr::null());
        gl.VertexAttribPointer(
            program.a_uv as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            stride,
            (2 * mem::size_of::<f32>()) as *const _,
        );
        gl.VertexAttribPointer(
            program.a_color as u32,
            4,
            ffi::FLOAT,
            ffi::FALSE,
            stride,
            (4 * mem::size_of::<f32>()) as *const _,
        );

        for primitive in self.primitives.iter().cloned() {
            let Primitive::Mesh(mesh) = primitive.primitive else {
                continue;
            };
            let Some(texture) = self.gl_textures.get(&mesh.texture_id) else {
                continue;
            };

            let clip = primitive.clip_rect;
            let x = (clip.min.x * scale).floor().max(0.0) as i32;
            let y_top = (clip.min.y * scale).floor().max(0.0) as i32;
            let x_max = (clip.max.x * scale).ceil().min(screen_size[0] * scale) as i32;
            let y_max = (clip.max.y * scale).ceil().min(screen_size[1] * scale) as i32;
            let w = (x_max - x).max(0);
            let h = (y_max - y_top).max(0);
            if w == 0 || h == 0 {
                continue;
            }
            gl.Scissor(x, output_height - y_max, w, h);
            gl.BindTexture(ffi::TEXTURE_2D, texture.id);

            for mesh in mesh.split_to_u16() {
                let vertices = mesh_vertices(&mesh.vertices);
                gl.BufferData(
                    ffi::ARRAY_BUFFER,
                    (vertices.len() * mem::size_of::<f32>()) as isize,
                    vertices.as_ptr() as *const _,
                    ffi::STREAM_DRAW,
                );
                gl.BufferData(
                    ffi::ELEMENT_ARRAY_BUFFER,
                    (mesh.indices.len() * mem::size_of::<u16>()) as isize,
                    mesh.indices.as_ptr() as *const _,
                    ffi::STREAM_DRAW,
                );
                gl.DrawElements(
                    ffi::TRIANGLES,
                    mesh.indices.len() as i32,
                    ffi::UNSIGNED_SHORT,
                    ptr::null(),
                );
            }
        }

        gl.DisableVertexAttribArray(program.a_pos as u32);
        gl.DisableVertexAttribArray(program.a_uv as u32);
        gl.DisableVertexAttribArray(program.a_color as u32);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
        gl.Disable(ffi::SCISSOR_TEST);
    }
}

fn pointer_button(button: EguiPointerButton) -> PointerButton {
    match button {
        EguiPointerButton::Primary => PointerButton::Primary,
        EguiPointerButton::Secondary => PointerButton::Secondary,
        EguiPointerButton::Middle => PointerButton::Middle,
        EguiPointerButton::Extra1 => PointerButton::Extra1,
        EguiPointerButton::Extra2 => PointerButton::Extra2,
    }
}

impl From<EguiModifiers> for Modifiers {
    fn from(value: EguiModifiers) -> Self {
        Self {
            alt: value.alt,
            ctrl: value.ctrl,
            shift: value.shift,
            mac_cmd: value.mac_cmd,
            command: value.command,
        }
    }
}

fn image_delta_rgba(image: &ImageData) -> Vec<u8> {
    match image {
        ImageData::Color(image) => image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect(),
        ImageData::Font(image) => image
            .srgba_pixels(None)
            .flat_map(|pixel| pixel.to_array())
            .collect(),
    }
}

fn mesh_vertices(vertices: &[egui::epaint::Vertex]) -> Vec<f32> {
    let mut out = Vec::with_capacity(vertices.len() * 8);
    for vertex in vertices {
        let [r, g, b, a] = vertex.color.to_array();
        out.extend_from_slice(&[
            vertex.pos.x,
            vertex.pos.y,
            vertex.uv.x,
            vertex.uv.y,
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        ]);
    }
    out
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn compile_egui_program(gl: &ffi::Gles2) -> Option<EguiGlProgram> {
    let vert = compile_shader(gl, ffi::VERTEX_SHADER, EGUI_VERT)?;
    let frag = compile_shader(gl, ffi::FRAGMENT_SHADER, EGUI_FRAG)?;
    let program = gl.CreateProgram();
    gl.AttachShader(program, vert);
    gl.AttachShader(program, frag);
    gl.LinkProgram(program);
    gl.DetachShader(program, vert);
    gl.DetachShader(program, frag);
    gl.DeleteShader(vert);
    gl.DeleteShader(frag);

    let mut status = ffi::FALSE as i32;
    gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status);
    if status == ffi::FALSE as i32 {
        gl.DeleteProgram(program);
        return None;
    }

    Some(EguiGlProgram {
        id: program,
        a_pos: gl.GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _),
        a_uv: gl.GetAttribLocation(program, b"a_uv\0".as_ptr() as *const _),
        a_color: gl.GetAttribLocation(program, b"a_color\0".as_ptr() as *const _),
        u_screen_size: gl.GetUniformLocation(program, b"u_screen_size\0".as_ptr() as *const _),
        u_tex: gl.GetUniformLocation(program, b"tex\0".as_ptr() as *const _),
    })
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn compile_shader(gl: &ffi::Gles2, variant: u32, src: &str) -> Option<u32> {
    let shader = gl.CreateShader(variant);
    if shader == 0 {
        return None;
    }
    gl.ShaderSource(
        shader,
        1,
        &(src.as_ptr() as *const i8),
        &(src.len() as i32),
    );
    gl.CompileShader(shader);

    let mut status = ffi::FALSE as i32;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut status);
    if status == ffi::FALSE as i32 {
        gl.DeleteShader(shader);
        return None;
    }
    Some(shader)
}

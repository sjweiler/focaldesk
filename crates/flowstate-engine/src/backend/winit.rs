// winit backend

use crate::backend::common::bootstrap_compositor_core;
use smithay::reexports::winit;
use smithay::backend::winit as winit_backend; // Smithay backend glue (has init, WinitEvent, etc.)
use smithay::backend::winit::{WinitEvent};    // (optional) import event type
use smithay::backend::input::{InputEvent, ButtonState};
use std::sync::Arc;
use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Renderer;
use smithay::utils::{Logical, Point, Rectangle, Physical, Transform, Size};
use smithay::backend::renderer::Frame;
use crate::core::wayland::client::ClientState;
use flowstate_flow::keybinds::BackendKind;

use crate::core::input::FlowInputEvent;

use crate::core::desktop::DesktopState;
use smithay::backend::winit::WinitEventLoop;
use crate::core::input::FlowKeyState;
use crate::core::input::FlowMouseButton;
use crate::core::input::FlowScrollDelta;
use smithay::backend::input::AbsolutePositionEvent;
use smithay::backend::input::{
    Axis, KeyState, KeyboardKeyEvent, PointerAxisEvent,
    PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent,
};
use crate::core::input::FlowModifiers;
use crate::core::backend_render::draw_output;
use crate::core::backend_render::prepare_output;
use flowstate_logging::flog;
use flowstate_types::OutputId;
use smithay::input::keyboard::keysyms;
use flowstate_themes::FlowThemeId;
use flowstate_themes::ThemeManager;
use flowstate_themes::theme::BuiltInThemeId;



pub fn translate_backend_input<B: smithay::backend::input::InputBackend>(
    input: &smithay::backend::input::InputEvent<B>,
    pointer_pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    clamp_rect: Rectangle<i32, Logical>,
    scale_factor: f64,
    modifiers: FlowModifiers,
) -> Option<FlowInputEvent> {
    
    


    match input {
        // 🔑 Keyboard
        InputEvent::Keyboard { event, .. } => {
            let state = match event.state() {
                KeyState::Pressed => FlowKeyState::Pressed,
                KeyState::Released => FlowKeyState::Released,
            };

           
            
            Some(FlowInputEvent::Key {
                keycode: event.key_code().into(),
                state,
                repeat: false,
                modifiers, // wire later if needed
            })
        }

        // 🖱 Pointer moved (libinput / most real devices: relative deltas)
        InputEvent::PointerMotion { event, .. } => {
            let pos: Point<f64, Logical> = pointer_pos + event.delta();
            let min_x = clamp_rect.loc.x as f64;
            let min_y = clamp_rect.loc.y as f64;
            let max_x = (clamp_rect.loc.x + clamp_rect.size.w) as f64 - f64::EPSILON;
            let max_y = (clamp_rect.loc.y + clamp_rect.size.h) as f64 - f64::EPSILON;
            Some(FlowInputEvent::PointerMoved {
                position: Point::from((
                    pos.x.clamp(min_x, max_x.max(min_x)),
                    pos.y.clamp(min_y, max_y.max(min_y)),
                )),
            })
        }

        // 🖱 Pointer moved (winit and some hosts: absolute normalized position)
        InputEvent::PointerMotionAbsolute { event, .. } => {
    let local = event
        .position_transformed(clamp_rect.size)
        .to_f64();

    // `position_transformed(clamp_rect.size)` already yields coordinates in the
    // logical-space extent we pass in, so dividing by scale again offsets hover
    // and hit-testing when scale != 1.0.
    let pos = Point::<f64, Logical>::from((
        clamp_rect.loc.x as f64 + local.x,
        clamp_rect.loc.y as f64 + local.y,
    ));

            Some(FlowInputEvent::PointerMoved {
                position: pos,
            })
        }

        // 🖱 Button
        InputEvent::PointerButton { event, .. } => {
            let button = match event.button_code() {
                0x110 => FlowMouseButton::Left,
                0x111 => FlowMouseButton::Right,
                0x112 => FlowMouseButton::Middle,
                0x113 => FlowMouseButton::Back,
                0x114 => FlowMouseButton::Forward,
                other => FlowMouseButton::Other(other as u16),
            };

            let state = match event.state() {
                ButtonState::Pressed => FlowKeyState::Pressed,
                ButtonState::Released => FlowKeyState::Released,
            };

            Some(FlowInputEvent::PointerButton {
                button,
                state,
                position: pointer_pos,
            })
        }

        // 🖱 Scroll
        InputEvent::PointerAxis { event, .. } => {
            let delta = if event.source() == smithay::backend::input::AxisSource::Wheel {
                FlowScrollDelta::Line {
                    x: event.amount(Axis::Horizontal).unwrap_or(0.0) as f32,
                    y: event.amount(Axis::Vertical).unwrap_or(0.0) as f32,
                }
            } else {
                FlowScrollDelta::Pixel {
                    x: event.amount(Axis::Horizontal).unwrap_or(0.0),
                    y: event.amount(Axis::Vertical).unwrap_or(0.0),
                }
            };

            Some(FlowInputEvent::PointerScroll {
                delta,
                position: pointer_pos,
            })
        }

        _ => None,
    }
}

fn dispatch_backend_events(
    state: &mut DesktopState,
    event_loop: &mut WinitEventLoop,
) -> anyhow::Result<bool> {
    let status = event_loop.dispatch_new_events(|event: WinitEvent| {
        match event {
            WinitEvent::Resized { size, scale_factor } => {
                state.winit_scale_factor = scale_factor;
                
                state.handle_input(FlowInputEvent::Resized {
                    output_id: OutputId(1),
                    width: size.w as u32,
                    height: size.h as u32,
                    scale_factor,
                });
            }

            WinitEvent::Input(input) => {
                let clamp_rect = Rectangle::from_loc_and_size(
                    (0, 0),
                    state
                        .outputs
                        .get(&state.primary_output)
                        .expect("active output missing")
                        .logical_size,
                );
                
                let scale_factor = state.winit_scale_factor;
                
                if let Some(event) = translate_backend_input(
                    &input,
                    state.input.pointer_pos,
                    clamp_rect,
                    scale_factor,
                    state.input.modifiers,
                ) {
                    state.handle_input(event);
                }
            }

            WinitEvent::CloseRequested => {
                state.running = false;
            }

            _ => {}
        }
    });

    Ok(
        state.running
            && matches!(
                status,
                smithay::reexports::winit::platform::pump_events::PumpStatus::Continue
            )
    )
}


pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    flog("FLOWSTATE: entered WINIT backend");
    // Create Smithay winit backend + renderer
    //let mut theme_manager =
   // ThemeManager::new(
    //    FlowThemeId::BuiltIn(BuiltInThemeId::Eagle)
   // );
    //let theme = theme_manager.active_theme();
    let (mut backend, mut event_loop) = winit_backend::init::<GlesRenderer>()?;
    
    backend.window().set_maximized(true);
    backend.window().set_decorations(false);
    // Ask the host compositor to focus our nested window immediately.
    // Some WMs will ignore this unless done from user interaction, but
    // requesting here avoids requiring an initial click on permissive setups.
    backend.window().focus_window();
    
    let size = backend.window_size();
    let scale = backend.scale_factor();
    let mut nested = bootstrap_compositor_core("flowstate-winit".to_string(), size, scale, BackendKind::Winit)?;

    let mut requested_focus_after_first_frame = false;
    while nested.state.running { 
    
        // dispatch events
        if !dispatch_backend_events(&mut nested.state, &mut event_loop)? {
            break;
        }
        
        // accept clients  
        if let Some(stream) = nested.listener.accept()? {
            println!("accepting client");
            let client = nested
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            nested.clients.push(client);
        }
        
        let now = Instant::now();
        let dt = now.saturating_duration_since(nested.last_now);

        {
            let (renderer, mut framebuffer) = backend.bind()?;
            nested.state.begin_portal_dispatch(
                renderer,
                &mut nested.ui_state,
                &mut nested.scene,
                &nested.output_state,
                now,
                dt,
            );
            nested.display.dispatch_clients(&mut nested.state)?;
            nested.state.end_portal_dispatch();
            std::mem::drop(framebuffer);
        }

        nested.display.flush_clients()?;

        //tick layout
        nested.state.tick_layout();
        // render if needed

        if nested.state.needs_redraw() {
            nested.last_now = now;

            let buffer_size = backend.window_size();
            {
                let (renderer, mut framebuffer) = backend.bind()?;

                let prepared = prepare_output(
                    &mut nested.state,
                    renderer,
                    OutputId(1),
                    buffer_size,
                    &mut nested.ui_state,
                    now,
                    dt,
                )?;

                let mut frame =
                    renderer.render(&mut framebuffer, buffer_size, Transform::Flipped180)?;

                

                draw_output(
                    &mut nested.state,
                    &mut frame,
                    &prepared,
                    &mut nested.ui_state,
                    &nested.scene,
                    &nested.output_state,
                )?;

                frame.finish()?;
            }

            backend.submit(None)?;

            if !requested_focus_after_first_frame {
                backend.window().focus_window();
                requested_focus_after_first_frame = true;
            }

            nested.state.clear_repaint_request();
            nested.state.render.frame_no += 1;

            let frame_time_ms = nested.start.elapsed().as_millis() as u32;
            nested.state.send_frame_callbacks(frame_time_ms);
        }
    }
    Ok(())
}    
 



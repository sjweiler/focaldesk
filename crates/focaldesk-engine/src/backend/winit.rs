#![allow(unused_imports)]

// winit backend

use crate::backend::common::{bootstrap_compositor_core, translate_backend_input, BootstrapOutput};
//use crate::backend::common::{
//    bootstrap_compositor_core, finish_xwayland_startup, start_xwayland, translate_backend_input,
//};
#[cfg(feature = "xwayland")]
use crate::backend::common::{finish_xwayland_startup, start_xwayland};

use smithay::backend::winit as winit_backend; // Smithay backend glue (has init, WinitEvent, etc.)
use smithay::backend::winit::WinitEvent; // (optional) import event type
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::winit;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::core::wayland::client::ClientState;
use focaldesk_flow::keybinds::BackendKind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::Renderer;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

use crate::core::input::FlowInputEvent;

use crate::core::backend_render::draw_output;
use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, prepare_output,
};
use crate::core::desktop::DesktopState;
use focaldesk_logging::{flog, flog_info};
use focaldesk_themes::theme::BuiltInThemeId;
use focaldesk_themes::FlowThemeId;
use focaldesk_themes::ThemeManager;
use focaldesk_types::OutputId;
use smithay::backend::winit::WinitEventLoop;
use smithay::input::keyboard::keysyms;
fn dispatch_backend_events(
    state: &mut DesktopState,
    event_loop: &mut WinitEventLoop,
) -> anyhow::Result<bool> {
    let status = event_loop.dispatch_new_events(|event: WinitEvent| match event {
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
            let clamp_rect = state.pointer_transform_rect_for_output(state.primary_output);

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
    });

    Ok(state.running
        && matches!(
            status,
            smithay::reexports::winit::event_loop::pump_events::PumpStatus::Continue
        ))
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    flog("FOCALDESK: entered WINIT backend");
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
    let mut nested = bootstrap_compositor_core(
        Some(BootstrapOutput {
            name: "focaldesk-winit".to_string(),
            buffer_size: size,
            scale_factor: scale,
        }),
        BackendKind::Winit,
    )?;

    #[cfg(feature = "xwayland")]
    let mut xwayland_event_loop = EventLoop::<DesktopState>::try_new()?;
    #[cfg(feature = "xwayland")]
    {
        start_xwayland(
            &mut nested.state,
            &nested.display.handle(),
            xwayland_event_loop.handle(),
        )?;
        finish_xwayland_startup(
            &mut xwayland_event_loop,
            &mut nested.display,
            &mut nested.state,
            Duration::from_secs(30),
        )?;
    }

    let mut requested_focus_after_first_frame = false;
    while nested.state.running {
        #[cfg(feature = "xwayland")]
        xwayland_event_loop.dispatch(Some(Duration::ZERO), &mut nested.state)?;

        nested.state.process_settings_ipc_requests();
        nested.state.process_chrome_timers();
        nested.state.process_notification_timers();
        nested.state.process_lock_timers();

        // dispatch events
        if !dispatch_backend_events(&mut nested.state, &mut event_loop)? {
            break;
        }

        // accept clients
        if let Some(stream) = nested.listener.accept()? {
            flog_info!("accepting client");
            let client = nested
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            nested.clients.push(client);
        }

        let now = Instant::now();
        let dt = now.saturating_duration_since(nested.last_now);

        {
            let (renderer, framebuffer) = backend.bind()?;
            nested.state.begin_portal_dispatch(
                renderer,
                &mut nested.ui_state,
                &mut nested.scene,
                &nested.output_state,
                now,
                dt,
            );
            if nested.state.wayland_clients_may_dispatch() {
                nested.display.dispatch_clients(&mut nested.state)?;
            }
            nested.state.end_portal_dispatch();
            std::mem::drop(framebuffer);
        }

        nested.state.refresh_space();
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
                    false,
                )?;

                let client_elements =
                    build_output_client_elements(&mut nested.state, renderer, OutputId(1));
                let popup_elements =
                    build_output_popup_elements(&mut nested.state, renderer, OutputId(1));

                let mut frame =
                    renderer.render(&mut framebuffer, buffer_size, Transform::Flipped180)?;

                draw_output(
                    &mut nested.state,
                    &mut frame,
                    &prepared,
                    &client_elements,
                    &popup_elements,
                    &mut nested.ui_state,
                    &nested.scene,
                    &nested.output_state,
                )?;

                let _ = frame.finish()?;
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

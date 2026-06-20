#![allow(unused_imports)]

// winit backend

use crate::backend::common::{bootstrap_compositor_core, translate_backend_input, BootstrapOutput};
#[cfg(feature = "xwayland")]
use crate::backend::common::{finish_xwayland_startup, start_xwayland};

use smithay::backend::winit as winit_backend;
use smithay::backend::winit::WinitEvent;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::winit;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::core::desktop::DesktopState;
use crate::core::linear_compositing::{
    present_offscreen_texture, render_output_offscreen, supports_linear_sdr, LinearOffscreenTargets,
};
use crate::core::portal::{
    complete_pending_portal_captures, complete_pending_portal_captures_for_output,
    publish_portal_capture_source,
};
use crate::core::wayland::client::ClientState;
use focaldesk_flow::keybinds::BackendKind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::Renderer;
use smithay::utils::{IsAlive, Physical, Size, Transform};

use crate::core::input::FlowInputEvent;
use focaldesk_logging::{flog, flog_info};
use focaldesk_types::OutputId;
use smithay::backend::winit::WinitEventLoop;

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
    let (mut backend, mut event_loop) = winit_backend::init::<GlesRenderer>()?;

    backend.window().set_maximized(true);
    backend.window().set_decorations(false);
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

    let buffer_size_phys = Size::<i32, Physical>::from((size.w, size.h));
    let mut render_targets = LinearOffscreenTargets {
        linear_supported: {
            let (renderer, _framebuffer) = backend.bind()?;
            let supported = supports_linear_sdr(renderer, buffer_size_phys);
            flog(&format!(
                "Linear SDR probe: winit format={:?} supported={supported}",
                crate::core::linear_compositing::LINEAR_SDR_FORMAT
            ));
            supported
        },
        ..LinearOffscreenTargets::default()
    };

    let mut requested_focus_after_first_frame = false;
    while nested.state.running {
        #[cfg(feature = "xwayland")]
        xwayland_event_loop.dispatch(Some(Duration::ZERO), &mut nested.state)?;

        nested.state.process_settings_ipc_requests();
        nested.state.process_chrome_timers();
        nested.state.process_notification_timers();
        nested.state.process_lock_timers();

        if !dispatch_backend_events(&mut nested.state, &mut event_loop)? {
            break;
        }

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

        nested.state.tick_layout();

        nested
            .state
            .image_copy_capture_sessions
            .retain(|session| session.alive());
        let portal_active = !nested.state.image_copy_capture_sessions.is_empty()
            && !nested.state.pending_portal_captures.is_empty();
        let should_render = nested.state.needs_redraw() || portal_active;

        if !should_render {
            if portal_active {
                let (renderer, _framebuffer) = backend.bind()?;
                complete_pending_portal_captures(
                    &mut nested.state,
                    renderer,
                    &mut nested.ui_state,
                    &nested.scene,
                    &nested.output_state,
                    now,
                    dt,
                );
            }
        } else {
            nested.last_now = now;

            let buffer_size = backend.window_size();
            let buffer_size_phys = Size::<i32, Physical>::from((buffer_size.w, buffer_size.h));
            {
                let (renderer, mut framebuffer) = backend.bind()?;

                let _sync = render_output_offscreen(
                    &mut nested.state,
                    renderer,
                    &mut render_targets,
                    OutputId(1),
                    buffer_size_phys,
                    &mut nested.ui_state,
                    &nested.scene,
                    &nested.output_state,
                    now,
                    dt,
                    false,
                )?;

                let offscreen = render_targets
                    .offscreen
                    .as_ref()
                    .ok_or("winit offscreen missing after render")?;
                let mut frame =
                    renderer.render(&mut framebuffer, buffer_size, Transform::Flipped180)?;
                present_offscreen_texture(
                    &mut frame,
                    &offscreen.texture,
                    buffer_size_phys,
                )?;
                let _ = frame.finish()?;

                publish_portal_capture_source(
                    &mut nested.state,
                    OutputId(1),
                    offscreen.texture.clone(),
                    buffer_size_phys,
                    now,
                );

                if portal_active {
                    complete_pending_portal_captures_for_output(
                        &mut nested.state,
                        renderer,
                        &mut nested.ui_state,
                        &nested.scene,
                        &nested.output_state,
                        OutputId(1),
                        now,
                        dt,
                    );
                }
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

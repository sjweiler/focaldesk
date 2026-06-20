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

use crate::core::linear_compositing::{
    present_offscreen_texture, run_linear_staged_pass, run_sdr_pass, supports_linear_sdr,
    use_linear_sdr_path, LinearOffscreenTargets,
};
use crate::core::wayland::client::ClientState;
use focaldesk_flow::keybinds::BackendKind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::Renderer;
use smithay::utils::{Physical, Size, Transform};

use crate::core::input::FlowInputEvent;

use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, prepare_output,
};
use crate::core::desktop::DesktopState;
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

                let srgb_to_linear = nested.state.render.chrome_shaders.srgb_to_linear.clone();
                let linear_to_srgb = nested.state.render.chrome_shaders.linear_to_srgb.clone();
                let use_linear = use_linear_sdr_path(renderer, &render_targets, buffer_size_phys)
                    && srgb_to_linear.is_some()
                    && linear_to_srgb.is_some();

                if use_linear {
                    if let Err(err) = render_targets.ensure_linear_offscreen(renderer, buffer_size_phys)
                    {
                        flog(&format!(
                            "Linear SDR disabled in winit after FP16 allocation failed: {err}"
                        ));
                    }
                }

                if use_linear && render_targets.linear_offscreen.is_some() {
            let _ = run_linear_staged_pass(
                        &mut nested.state,
                        renderer,
                        &mut render_targets,
                        buffer_size_phys,
                        &prepared,
                        &client_elements,
                        &popup_elements,
                        &mut nested.ui_state,
                        &nested.scene,
                        &nested.output_state,
                        srgb_to_linear.as_ref().unwrap(),
                        linear_to_srgb.as_ref().unwrap(),
                    )?;
                    let offscreen = render_targets
                        .offscreen
                        .as_ref()
                        .ok_or("winit offscreen missing after linear pass")?;
                    let mut frame =
                        renderer.render(&mut framebuffer, buffer_size, Transform::Flipped180)?;
                    present_offscreen_texture(
                        &mut frame,
                        &offscreen.texture,
                        buffer_size_phys,
                    )?;
                    let _ = frame.finish()?;
                } else {
            let _ = run_sdr_pass(
                        &mut nested.state,
                        renderer,
                        &mut render_targets,
                        buffer_size_phys,
                        &prepared,
                        &client_elements,
                        &popup_elements,
                        &mut nested.ui_state,
                        &nested.scene,
                        &nested.output_state,
                    )?;
                    let offscreen = render_targets
                        .offscreen
                        .as_ref()
                        .ok_or("winit offscreen missing after SDR pass")?;
                    let mut frame =
                        renderer.render(&mut framebuffer, buffer_size, Transform::Flipped180)?;
                    present_offscreen_texture(
                        &mut frame,
                        &offscreen.texture,
                        buffer_size_phys,
                    )?;
                    let _ = frame.finish()?;
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

//! Raw Vulkan DRM backend: Smithay/libseat own KMS, GBM owns scanout, and
//! ash only renders into DMA-BUFs and exports explicit fences.

use std::collections::HashSet;
use std::error::Error;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_logging::{flog, flog_warn};
use focaldesk_render::{AshDrmRenderer, DrmRenderTarget};
use focaldesk_types::OutputId;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{
    gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
    Buffer as _, Format, Fourcc, Modifier,
};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, GbmBufferedSurface};
use smithay::backend::input::{
    InputEvent, KeyState, KeyboardKeyEvent, SwitchState, SwitchToggleEvent,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::sync::{Fence, Interrupted, SyncPoint};
use smithay::backend::session::{libseat::LibSeatSession, Event as SessionEvent, Session};
use smithay::backend::udev::primary_gpu;
use smithay::output::{Mode as WlMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::drm::control::{connector, crtc, plane, Device as _, Mode};
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::input::event::switch::Switch as InputSwitch;
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState};

use super::common::{
    bootstrap_compositor_core, client_state_from_stream, is_nonfatal_wayland_io_error,
    physical_size_mm_from_pixels, stop_focaldesk_session_target, NestedDesktop,
};
use super::drm::{
    configured_display_scale, dispatch_backend_input_event, load_display_config,
    select_connector_mode,
};
use super::wgpu_nested::VulkanSceneBuilder;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SCANOUT_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb8888,
    Fourcc::Argb8888,
    Fourcc::Xbgr8888,
    Fourcc::Abgr8888,
];
type VulkanScanout = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>;

#[derive(Debug)]
struct KmsFence(OwnedFd);

impl Fence for KmsFence {
    fn is_signaled(&self) -> bool {
        let mut pollfd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pollfd, 1, 0) > 0 }
    }

    fn wait(&self) -> std::result::Result<(), Interrupted> {
        loop {
            let mut pollfd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut pollfd, 1, -1) };
            if result > 0 {
                return Ok(());
            }
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return Err(Interrupted);
        }
    }

    fn is_exportable(&self) -> bool {
        true
    }
    fn export(&self) -> Option<OwnedFd> {
        self.0.try_clone().ok()
    }
}

struct OutputConfig {
    name: String,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    width: u32,
    height: u32,
    scale: f64,
    origin: Point<i32, Logical>,
    primary: bool,
    output_id: OutputId,
}

struct VulkanOutput {
    name: String,
    width: u32,
    height: u32,
    output_id: OutputId,
    crtc: crtc::Handle,
    scanout: VulkanScanout,
    frame_pending: bool,
}

struct VulkanDrmData {
    desktop: NestedDesktop,
    renderer: AshDrmRenderer,
    scene_builder: VulkanSceneBuilder,
    drm: DrmDevice,
    outputs: Vec<VulkanOutput>,
    libinput: Libinput,
    session_active: bool,
    fatal_error: Option<anyhow::Error>,
    surface_blocker_loop: EventLoop<'static, crate::core::desktop::DesktopState>,
}

fn select_outputs(drm: &DrmDevice) -> Result<Vec<OutputConfig>> {
    let resources = drm.resource_handles().context("query DRM resources")?;
    let configured = load_display_config();
    let mut used_crtcs = HashSet::new();
    let mut outputs = Vec::new();
    let mut next_x = 0;
    for handle in resources.connectors() {
        let info = drm
            .get_connector(*handle, true)
            .context("query DRM connector")?;
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let saved = configured.iter().find(|display| display.name == name);
        if saved.is_some_and(|display| !display.enabled) {
            flog(format!(
                "Raw Vulkan DRM leaving configured-disabled output {name} off"
            ));
            continue;
        }
        let requested_mode =
            saved.map(|display| (display.mode_width, display.mode_height, display.refresh_mhz));
        let mode = select_connector_mode(info.modes(), requested_mode)
            .context("connected DRM output has no mode")?;
        let selected_crtc = info.encoders().iter().find_map(|encoder| {
            let encoder = drm.get_encoder(*encoder).ok()?;
            resources
                .filter_crtcs(encoder.possible_crtcs())
                .into_iter()
                .find(|candidate| !used_crtcs.contains(candidate))
        });
        let crtc = selected_crtc.context("connected DRM output has no compatible CRTC")?;
        used_crtcs.insert(crtc);
        let (width, height) = mode.size();
        let scale = configured_display_scale(&configured, &name);
        let logical_width = (f64::from(width) / scale).round() as i32;
        let origin = saved
            .map(|display| Point::from((display.logical_x, display.logical_y)))
            .unwrap_or_else(|| Point::from((next_x, 0)));
        next_x = next_x.max(origin.x + logical_width);
        outputs.push(OutputConfig {
            name,
            connector: *handle,
            crtc,
            mode,
            width: u32::from(width),
            height: u32::from(height),
            scale,
            origin,
            primary: saved.is_some_and(|display| display.primary),
            output_id: OutputId(outputs.len() as u64 + 1),
        });
    }
    if outputs.is_empty() {
        Err(anyhow!(
            "no enabled connected DRM output with a usable mode"
        ))
    } else {
        Ok(outputs)
    }
}

fn target_from_dmabuf(dmabuf: &Dmabuf) -> Result<DrmRenderTarget> {
    let format = dmabuf.format();
    Ok(DrmRenderTarget {
        planes: dmabuf
            .handles()
            .map(|fd| fd.try_clone_to_owned().map(Arc::new))
            .collect::<std::io::Result<Vec<_>>>()?,
        offsets: dmabuf.offsets().collect(),
        strides: dmabuf.strides().collect(),
        fourcc: format.code as u32,
        modifier: u64::from(format.modifier),
        width: dmabuf.width(),
        height: dmabuf.height(),
    })
}

fn ash_formats(renderer: &AshDrmRenderer) -> Vec<Format> {
    renderer
        .render_formats()
        .into_iter()
        .filter_map(|(code, modifier)| {
            Fourcc::try_from(code).ok().map(|code| Format {
                code,
                modifier: Modifier::from(modifier),
            })
        })
        .collect()
}

fn plane_has_input_fence(drm: &DrmDevice, plane: plane::Handle) -> Result<bool> {
    let properties = drm
        .get_properties(plane)
        .context("query primary-plane properties")?;
    Ok(properties.as_props_and_values().0.iter().any(|property| {
        drm.get_property(*property)
            .is_ok_and(|info| info.name().to_bytes() == b"IN_FENCE_FD")
    }))
}

fn advertise_dmabuf(
    desktop: &mut NestedDesktop,
    renderer: &AshDrmRenderer,
    node: DrmNode,
) -> Result<()> {
    let formats = renderer
        .sample_formats()
        .into_iter()
        .filter_map(|(code, modifier)| {
            Fourcc::try_from(code).ok().map(|code| Format {
                code,
                modifier: Modifier::from(modifier),
            })
        })
        .collect::<Vec<_>>();
    let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats.iter().copied()).build()?;
    let global = desktop
        .state
        .dmabuf_state
        .create_global_with_default_feedback::<crate::core::desktop::DesktopState>(
            &desktop.display.handle(),
            &feedback,
        );
    desktop.state.dmabuf_global = Some(global);
    desktop.state.dmabuf_node = Some(node);
    desktop.state.wgpu_dmabuf_formats = formats;
    Ok(())
}

/// Run the raw-Vulkan compositor with Smithay/libseat KMS ownership and one
/// explicit-sync GBM swapchain per configured output.
pub fn run() -> Result<(), Box<dyn Error>> {
    flog_warn!("FOCALDESK: entered raw ash DRM backend (Smithay KMS + GBM scanout)");
    let mut event_loop: EventLoop<VulkanDrmData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();
    let (mut session, session_notifier) =
        LibSeatSession::new().map_err(|error| anyhow!("initialize libseat: {error}"))?;
    let primary_path: PathBuf = primary_gpu(session.seat())?
        .ok_or_else(|| anyhow!("no primary GPU found for seat {}", session.seat()))?;
    let node = DrmNode::from_path(&primary_path).context("identify primary DRM node")?;
    let fd = session
        .open(&primary_path, OFlags::RDWR | OFlags::CLOEXEC)
        .with_context(|| format!("open primary DRM node {}", primary_path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let output_configs = select_outputs(&drm)?;
    let renderer = AshDrmRenderer::new(node.major(), node.minor())?;
    let render_formats = ash_formats(&renderer);
    if render_formats.is_empty() {
        return Err(anyhow!("Vulkan device exposes no renderable DRM scanout modifiers").into());
    }
    let has_syncobj = drm
        .get_driver_capability(DriverCapability::SyncObj)
        .is_ok_and(|supported| supported != 0);
    if !drm.is_atomic() || !has_syncobj {
        return Err(anyhow!(
            "raw Vulkan DRM requires atomic KMS and DRM syncobj; refusing an implicit blocking fallback"
        ).into());
    }

    let mut desktop = bootstrap_compositor_core(None, BackendKind::Drm)?;
    let mut outputs = Vec::with_capacity(output_configs.len());
    let mut configured_primary = None;
    for config in output_configs {
        let drm_surface = drm.create_surface(config.crtc, config.mode, &[config.connector])?;
        let gbm = GbmDevice::new(fd.clone())
            .with_context(|| format!("create GBM device for Vulkan output {}", config.name))?;
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let scanout = VulkanScanout::new(
            drm_surface,
            allocator,
            &SCANOUT_FORMATS,
            render_formats.clone(),
        )?;
        if !plane_has_input_fence(&drm, scanout.plane())? {
            return Err(anyhow!(
                "raw Vulkan DRM output {} has no primary-plane IN_FENCE_FD",
                config.name
            )
            .into());
        }

        let physical_size =
            Size::<i32, Physical>::from((config.width as i32, config.height as i32));
        let (mm_width, mm_height) = physical_size_mm_from_pixels(physical_size);
        let output = Output::new(
            config.name.clone(),
            PhysicalProperties {
                size: (mm_width, mm_height).into(),
                subpixel: Subpixel::Unknown,
                make: "FocalDesk".into(),
                model: "Vulkan DRM".into(),
                serial_number: config.name.clone(),
            },
        );
        let wl_mode = WlMode {
            size: (config.width as i32, config.height as i32).into(),
            refresh: (config.mode.vrefresh() as i32).max(1) * 1000,
        };
        output.change_current_state(
            Some(wl_mode),
            Some(Transform::Normal),
            Some(OutputScale::Custom {
                advertised_integer: config.scale.round().max(1.0) as i32,
                fractional: config.scale,
            }),
            Some(config.origin),
        );
        output.set_preferred(wl_mode);
        output.create_global::<crate::core::desktop::DesktopState>(&desktop.display.handle());
        desktop.state.register_output_entry(
            config.output_id,
            output,
            config.origin,
            physical_size,
            config.scale,
        );
        if config.primary {
            configured_primary = Some(config.output_id);
        }
        flog(format!(
            "Raw Vulkan DRM output configured: {} {}x{}@{}Hz scale={} origin={},{}",
            config.name,
            config.width,
            config.height,
            config.mode.vrefresh(),
            config.scale,
            config.origin.x,
            config.origin.y,
        ));
        outputs.push(VulkanOutput {
            name: config.name,
            width: config.width,
            height: config.height,
            output_id: config.output_id,
            crtc: config.crtc,
            scanout,
            frame_pending: false,
        });
    }
    desktop.state.primary_output = configured_primary.unwrap_or(outputs[0].output_id);
    desktop.state.focused_output = desktop.state.primary_output;
    desktop.state.mark_redraw();
    advertise_dmabuf(&mut desktop, &renderer, node)?;
    if let Ok(sync_fd) = fd.as_fd().try_clone_to_owned() {
        let sync_device = DrmDeviceFd::new(DeviceFd::from(sync_fd));
        if supports_syncobj_eventfd(&sync_device) {
            desktop.state.drm_syncobj_state = Some(DrmSyncobjState::new::<
                crate::core::desktop::DesktopState,
            >(
                &desktop.display.handle(), sync_device
            ));
        }
    }
    let surface_blocker_loop = EventLoop::try_new()?;
    desktop.state.surface_blocker_loop_handle = Some(surface_blocker_loop.handle());

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|error| anyhow!("assign libinput seat: {error:?}"))?;
    let input_backend = LibinputInputBackend::new(libinput.clone());
    let mut data = VulkanDrmData {
        desktop,
        renderer,
        scene_builder: VulkanSceneBuilder::default(),
        drm,
        outputs,
        libinput,
        session_active: session.is_active(),
        fatal_error: None,
        surface_blocker_loop,
    };

    let _input_token = loop_handle.insert_source(input_backend, move |event, _, data| {
        if let InputEvent::Keyboard { event, .. } = &event {
            if event.state() == KeyState::Pressed {
                let keycode: u32 = event.key_code().into();
                let mods = data.desktop.state.input.modifiers;
                if mods.ctrl && mods.alt {
                    let vt = match keycode {
                        67..=76 => Some((keycode - 67 + 1) as i32),
                        95 => Some(11),
                        96 => Some(12),
                        _ => None,
                    };
                    if let Some(vt) = vt {
                        if let Err(error) = session.change_vt(vt) {
                            flog_warn!("VT switch to {vt} failed: {error:?}");
                        }
                        return;
                    }
                }
            }
        }
        if let InputEvent::SwitchToggle { event, .. } = &event {
            if matches!(event.switch(), Some(InputSwitch::Lid)) {
                data.desktop
                    .state
                    .handle_lid_switch(matches!(event.state(), SwitchState::On));
            }
        }
        dispatch_backend_input_event::<LibinputInputBackend>(&mut data.desktop.state, &event);
    })?;

    let _drm_token = loop_handle.insert_source(drm_notifier, |event, _, data| match event {
        DrmEvent::VBlank(crtc) => {
            let Some(output) = data.outputs.iter_mut().find(|output| output.crtc == crtc) else {
                return;
            };
            match output.scanout.frame_submitted() {
                Ok(_) => output.frame_pending = false,
                Err(error) => {
                    data.fatal_error = Some(anyhow!(
                        "complete Vulkan DRM frame on {}: {error}",
                        output.name
                    ));
                }
            }
        }
        DrmEvent::Error(error) => data.fatal_error = Some(anyhow!("DRM event error: {error}")),
    })?;

    let _session_token =
        loop_handle.insert_source(session_notifier, |event, _, data| match event {
            SessionEvent::PauseSession => {
                data.session_active = false;
                for output in &mut data.outputs {
                    if output.frame_pending {
                        if let Err(error) = output.scanout.frame_submitted() {
                            flog_warn!(
                                "retiring pre-pause Vulkan frame on {} failed: {error}",
                                output.name
                            );
                        }
                        output.frame_pending = false;
                    }
                }
                data.drm.pause();
                data.libinput.suspend();
                data.desktop.state.handle_session_suspend();
                flog_warn!("raw Vulkan DRM session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = data.drm.activate(true) {
                    data.fatal_error = Some(anyhow!("reactivate DRM device: {error}"));
                    return;
                }
                for output in &mut data.outputs {
                    output.scanout.reset_buffers();
                    output.frame_pending = false;
                }
                if let Err(error) = data.libinput.resume() {
                    data.fatal_error = Some(anyhow!("resume libinput: {error:?}"));
                    return;
                }
                data.session_active = true;
                data.desktop.state.handle_session_resume();
                data.desktop.state.mark_redraw();
                flog("raw Vulkan DRM session resumed");
            }
        })?;

    flog_warn!(
        "Raw Vulkan DRM ready: outputs={} renderer={} (no Vulkan display WSI)",
        data.outputs.len(),
        data.renderer.info().adapter_name
    );
    while data.desktop.state.running {
        event_loop.dispatch(Some(FRAME_INTERVAL), &mut data)?;
        data.surface_blocker_loop
            .dispatch(Some(Duration::ZERO), &mut data.desktop.state)?;
        data.renderer.poll()?;
        if let Some(error) = data.fatal_error.take() {
            return Err(error.into());
        }
        if !data.session_active {
            continue;
        }
        while let Some(stream) = data.desktop.listener.accept()? {
            let client_state = client_state_from_stream(&stream);
            let client = data
                .desktop
                .display
                .handle()
                .insert_client(stream, Arc::new(client_state))?;
            data.desktop.clients.push(client);
        }
        if data.desktop.state.wayland_clients_may_dispatch() {
            if let Err(error) = data
                .desktop
                .display
                .dispatch_clients(&mut data.desktop.state)
            {
                if !is_nonfatal_wayland_io_error(&error) {
                    return Err(error.into());
                }
            }
        }
        data.desktop.state.process_deferred_window_ops();
        data.desktop.state.refresh_space();
        data.desktop.state.tick_layout();
        data.desktop.display.flush_clients()?;
        if !data.desktop.state.needs_redraw()
            && !data.outputs.iter().any(|output| {
                data.desktop
                    .state
                    .output_has_pending_damage(output.output_id)
            })
        {
            continue;
        }
        data.desktop.state.materialize_full_redraw_damage();

        let mut presented_any = false;
        for index in 0..data.outputs.len() {
            let output_id = data.outputs[index].output_id;
            if data.outputs[index].frame_pending
                || !data.desktop.state.output_has_pending_damage(output_id)
            {
                continue;
            }

            let scene = data
                .scene_builder
                .build_for_output(&mut data.desktop.state, output_id);
            let output = &mut data.outputs[index];
            let (dmabuf, _) = match output.scanout.next_buffer() {
                Ok(next) => next,
                Err(error) => {
                    flog_warn!("Vulkan GBM acquire skipped on {}: {error}", output.name);
                    continue;
                }
            };
            let target = target_from_dmabuf(&dmabuf)?;
            let submission = data.renderer.render(
                &target,
                &scene.background,
                &scene.surfaces,
                &scene.overlay,
                scene.overlay_after_surface,
                &scene.damage,
            )?;
            let sync = SyncPoint::from(KmsFence(submission.fence_fd));
            let full_damage = vec![Rectangle::from_loc_and_size(
                (0, 0),
                (output.width as i32, output.height as i32),
            )];
            match output
                .scanout
                .queue_buffer(Some(sync), Some(full_damage), ())
            {
                Ok(()) => {
                    output.frame_pending = true;
                    data.desktop.state.clear_output_repaint_request(output_id);
                    presented_any = true;
                }
                Err(error) => {
                    data.desktop.state.mark_output_full_damage(
                        output_id,
                        crate::core::desktop::DamageSource::Unknown,
                    );
                    flog_warn!("Vulkan atomic KMS queue failed on {}: {error}", output.name);
                }
            }
        }
        if presented_any {
            data.desktop.state.render.frame_no += 1;
            data.desktop
                .state
                .send_frame_callbacks(data.desktop.start.elapsed().as_millis() as u32);
        }
        data.desktop.state.finish_output_repaint_cycle();
    }
    stop_focaldesk_session_target();
    Ok(())
}

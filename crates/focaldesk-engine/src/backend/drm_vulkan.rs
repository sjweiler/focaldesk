//! Raw Vulkan DRM backend: Smithay/libseat own KMS, GBM owns scanout, and
//! ash only renders into DMA-BUFs and exports explicit fences.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_logging::{flog, flog_warn};
use focaldesk_render::{
    AshDrmCapture, AshDrmHdrOutput, AshDrmOutputLut, AshDrmRenderer, AshDrmTransfer,
    DrmRenderTarget,
};
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
use smithay::backend::udev::{primary_gpu, UdevBackend, UdevEvent};
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
    physical_size_mm_from_pixels, pump_desktop_services, spawn_session_sleep_watch,
    stop_focaldesk_session_target, NestedDesktop, SessionSleepEvent,
};
#[cfg(feature = "xwayland")]
use super::common::{finish_xwayland_startup, start_xwayland};
use super::drm::{
    configured_display_hdr_requested, configured_display_scale, configured_hdr_appearance,
    connector_edid, dispatch_backend_input_event, hdr_detection, load_display_config,
    parse_edid_identity, select_connector_mode, HdrSupport,
};
use super::wgpu_nested::VulkanSceneBuilder;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const KMS_PRESENT_TIMEOUT: Duration = Duration::from_secs(5);
const SDR_SCANOUT_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb8888,
    Fourcc::Argb8888,
    Fourcc::Xbgr8888,
    Fourcc::Abgr8888,
];
const HDR_SCANOUT_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb2101010,
    Fourcc::Argb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Abgr2101010,
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
    physical_size_mm: (i32, i32),
    make: String,
    model: String,
    serial_number: String,
    edid: Option<Vec<u8>>,
    color_profile: focaldesk_settings_core::DisplayColorProfile,
    icc_profile_path: Option<String>,
    hdr_support: HdrSupport,
    hdr_requested: bool,
    hdr_appearance: focaldesk_settings_core::HdrAppearance,
}

struct VulkanOutput {
    name: String,
    width: u32,
    height: u32,
    output_id: OutputId,
    origin: Point<i32, Logical>,
    crtc: crtc::Handle,
    scanout: VulkanScanout,
    frame_pending: bool,
    present_started_at: Option<Instant>,
    hdr_metadata_blob: Option<u64>,
}

struct VulkanDrmData {
    desktop: NestedDesktop,
    #[cfg(feature = "xwayland")]
    xwayland_event_loop: EventLoop<'static, crate::core::desktop::DesktopState>,
    renderer: AshDrmRenderer,
    scene_builder: VulkanSceneBuilder,
    drm: DrmDevice,
    drm_fd: DrmDeviceFd,
    primary_node: DrmNode,
    session: LibSeatSession,
    outputs: Vec<VulkanOutput>,
    libinput: Libinput,
    session_active: bool,
    resume_pending: bool,
    resume_retry_at: Option<Instant>,
    topology_refresh_pending: bool,
    capture_pending: HashSet<OutputId>,
    screenshot_all_captures: HashMap<OutputId, (u32, u32, Vec<u8>)>,
    fatal_error: Option<anyhow::Error>,
    surface_blocker_loop: EventLoop<'static, crate::core::desktop::DesktopState>,
}

fn pause_vulkan_session(data: &mut VulkanDrmData, reason: &str) {
    if !data.session_active {
        return;
    }
    data.resume_pending = false;
    data.resume_retry_at = None;
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
            output.present_started_at = None;
        }
    }
    data.drm.pause();
    data.libinput.suspend();
    data.desktop.state.handle_session_suspend();
    flog_warn!("raw Vulkan DRM session paused: {reason}");
}

fn resume_vulkan_session(data: &mut VulkanDrmData, reason: &str) -> Result<()> {
    if !data.session.is_active() {
        return Err(anyhow!("libseat has not restored device ownership"));
    }
    data.drm.activate(true).context("reactivate DRM device")?;
    for output in &mut data.outputs {
        output.scanout.reset_buffers();
        output.frame_pending = false;
        output.present_started_at = None;
    }
    data.libinput
        .resume()
        .map_err(|()| anyhow!("resume libinput"))?;
    data.session_active = true;
    data.resume_pending = false;
    data.resume_retry_at = None;
    data.topology_refresh_pending = true;
    data.desktop.state.handle_session_resume();
    data.desktop.state.mark_redraw();
    flog(format!("raw Vulkan DRM session resumed: {reason}"));
    Ok(())
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
        let fallback_mm = physical_size_mm_from_pixels(Size::<i32, Physical>::from((
            width as i32,
            height as i32,
        )));
        let physical_size_mm = info
            .size()
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map(|(width, height)| (width as i32, height as i32))
            .unwrap_or(fallback_mm);
        let edid = connector_edid(drm, *handle);
        let hdr_support = hdr_detection::connector_hdr_support(drm, *handle, edid.as_deref());
        let hdr_requested = configured_display_hdr_requested(&configured, &name);
        let hdr_appearance = configured_hdr_appearance(&configured, &name);
        let identity = edid.as_deref().and_then(parse_edid_identity);
        let make = identity
            .as_ref()
            .map(|identity| identity.make.clone())
            .unwrap_or_else(|| "FocalDesk".to_string());
        let model = identity
            .as_ref()
            .map(|identity| identity.model.clone())
            .unwrap_or_else(|| info.interface().as_str().to_string());
        let serial_number = identity
            .as_ref()
            .map(|identity| identity.serial_number.clone())
            .unwrap_or_else(|| name.clone());
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
            physical_size_mm,
            make,
            model,
            serial_number,
            edid,
            color_profile: saved
                .map(|display| display.color_profile)
                .unwrap_or_default(),
            icc_profile_path: saved.and_then(|display| display.icc_profile_path.clone()),
            hdr_support,
            hdr_requested,
            hdr_appearance,
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

fn initialize_vulkan_outputs(
    desktop: &mut NestedDesktop,
    renderer: &AshDrmRenderer,
    drm: &mut DrmDevice,
    fd: &DrmDeviceFd,
    output_configs: Vec<OutputConfig>,
) -> Result<Vec<VulkanOutput>> {
    let render_formats = ash_formats(renderer);
    let mut outputs = Vec::with_capacity(output_configs.len());
    let mut configured_primary = None;
    for config in output_configs {
        let drm_surface = drm.create_surface(config.crtc, config.mode, &[config.connector])?;
        let gbm = GbmDevice::new(fd.clone())
            .with_context(|| format!("create GBM device for Vulkan output {}", config.name))?;
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let prefer_hdr = config.hdr_requested
            && config.hdr_support.can_signal_hdr10()
            && config.hdr_support.bpc_control_allows_ten_bit();
        let mut scanout_formats = Vec::with_capacity(8);
        if prefer_hdr {
            scanout_formats.extend(HDR_SCANOUT_FORMATS);
        }
        scanout_formats.extend(SDR_SCANOUT_FORMATS);
        let scanout = VulkanScanout::new(
            drm_surface,
            allocator,
            &scanout_formats,
            render_formats.clone(),
        )?;
        if !plane_has_input_fence(drm, scanout.plane())? {
            return Err(anyhow!(
                "raw Vulkan DRM output {} has no primary-plane IN_FENCE_FD",
                config.name
            ));
        }

        let physical_size =
            Size::<i32, Physical>::from((config.width as i32, config.height as i32));
        let output = Output::new(
            config.name.clone(),
            PhysicalProperties {
                size: config.physical_size_mm.into(),
                subpixel: Subpixel::Unknown,
                make: config.make.clone(),
                model: config.model.clone(),
                serial_number: config.serial_number.clone(),
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
        desktop.state.set_output_monitor_identity(
            config.output_id,
            config.make,
            config.model,
            config.serial_number,
            config.edid,
        );
        let ten_bit_scanout = HDR_SCANOUT_FORMATS.contains(&scanout.format());
        let mut hdr_active = false;
        let hdr_metadata_blob = if prefer_hdr && ten_bit_scanout {
            let staged = (|| -> Result<u64> {
                let metadata = hdr_detection::hdr_kms::create_hdr_metadata_blob(
                    drm,
                    &config.hdr_support,
                    hdr_detection::hdr_kms::hdr_metadata_config(config.hdr_appearance),
                )?;
                let state = match hdr_detection::hdr_kms::build_connector_hdr_state(
                    drm,
                    config.connector,
                    &config.hdr_support,
                    true,
                    Some(metadata),
                )? {
                    Some(state) => state,
                    None => {
                        hdr_detection::hdr_kms::destroy_hdr_metadata_blob(drm, Some(metadata));
                        return Err(anyhow!("HDR connector exposes no programmable KMS state"));
                    }
                };
                if let Err(error) = scanout.surface().use_hdr_state(state) {
                    hdr_detection::hdr_kms::destroy_hdr_metadata_blob(drm, Some(metadata));
                    return Err(anyhow!("queue raw Vulkan HDR KMS state: {error}"));
                }
                Ok(metadata)
            })();
            match staged {
                Ok(metadata) => {
                    hdr_active = true;
                    Some(metadata)
                }
                Err(error) => {
                    flog_warn!(
                        "Raw Vulkan HDR setup failed on {}; using SDR: {error:#}",
                        config.name
                    );
                    None
                }
            }
        } else {
            if config.hdr_requested {
                flog_warn!(
                    "Raw Vulkan HDR unavailable on {}; using SDR (signaling={} ten_bit_scanout={ten_bit_scanout})",
                    config.name,
                    config.hdr_support.can_signal_hdr10(),
                );
            }
            None
        };
        if let Some(output) = desktop.state.outputs.get_mut(&config.output_id) {
            output.color_profile_override = config.color_profile;
            output.icc_profile_path = config.icc_profile_path;
            output.hdr_supported = config.hdr_support.is_detected();
            output.hdr_requested = config.hdr_requested && output.hdr_supported;
            output.hdr_kms_applied = hdr_active;
            output.hdr_enabled = hdr_active;
            output.hdr_appearance = config.hdr_appearance.validate().unwrap_or_default();
        }
        desktop.state.refresh_output_color(config.output_id);
        if config.primary {
            configured_primary = Some(config.output_id);
        }
        flog(format!(
            "Raw Vulkan DRM output configured: {} {}x{}@{}Hz scale={} origin={},{} format={:?} hdr={hdr_active}",
            config.name,
            config.width,
            config.height,
            config.mode.vrefresh(),
            config.scale,
            config.origin.x,
            config.origin.y,
            scanout.format(),
        ));
        outputs.push(VulkanOutput {
            name: config.name,
            width: config.width,
            height: config.height,
            output_id: config.output_id,
            origin: config.origin,
            crtc: config.crtc,
            scanout,
            frame_pending: false,
            present_started_at: None,
            hdr_metadata_blob,
        });
    }
    desktop.state.primary_output = configured_primary.unwrap_or(outputs[0].output_id);
    desktop.state.focused_output = desktop.state.primary_output;
    desktop.state.mark_redraw();
    Ok(outputs)
}

fn rebuild_vulkan_outputs(data: &mut VulkanDrmData) -> Result<bool> {
    // Probe first so a transient connector read failure never destroys the
    // currently working topology.
    let output_configs = match select_outputs(&data.drm) {
        Ok(configs) => configs,
        Err(error) => {
            flog_warn!("raw Vulkan DRM topology rebuild deferred: {error:#}");
            return Ok(false);
        }
    };
    let snapshot = data.desktop.state.snapshot_output_topology();
    crate::core::portal::invalidate_portal_output_state(&mut data.desktop.state);
    for output in &mut data.outputs {
        if output.frame_pending {
            let _ = output.scanout.frame_submitted();
        }
        if let Some(state) = data.desktop.state.outputs.get(&output.output_id) {
            data.desktop.state.space.unmap_output(&state.handle);
        }
        hdr_detection::hdr_kms::destroy_hdr_metadata_blob(
            &data.drm,
            output.hdr_metadata_blob.take(),
        );
        data.desktop.state.outputs.shift_remove(&output.output_id);
        data.desktop
            .state
            .desktop_outputs
            .shift_remove(&output.output_id);
        data.desktop
            .output_state
            .outputs
            .shift_remove(&output.output_id);
    }
    data.outputs.clear();
    data.capture_pending.clear();
    data.screenshot_all_captures.clear();
    data.scene_builder.clear();
    data.outputs = initialize_vulkan_outputs(
        &mut data.desktop,
        &data.renderer,
        &mut data.drm,
        &data.drm_fd,
        output_configs,
    )?;
    data.desktop.state.restore_output_topology(snapshot);
    data.desktop.state.mark_redraw();
    Ok(true)
}

fn save_vulkan_screenshot(
    data: &mut VulkanDrmData,
    output_id: OutputId,
    width: u32,
    height: u32,
    rgba: &[u8],
) {
    let name = data
        .outputs
        .iter()
        .find(|output| output.output_id == output_id)
        .map(|output| output.name.as_str())
        .unwrap_or("output");
    data.desktop.state.screenshot_seq += 1;
    match crate::core::screenshot::save_srgb_rgba8_screenshot(
        width,
        height,
        rgba,
        name,
        data.desktop.state.screenshot_seq,
    ) {
        Ok(path) => flog(format!("Screenshot saved to {}", path.display())),
        Err(error) => flog_warn!("Vulkan screenshot failed: {error:#}"),
    }
}

fn finish_all_outputs_screenshot(data: &mut VulkanDrmData) {
    if data.outputs.is_empty()
        || data
            .outputs
            .iter()
            .any(|output| !data.screenshot_all_captures.contains_key(&output.output_id))
    {
        return;
    }
    let min_x = data
        .outputs
        .iter()
        .map(|output| output.origin.x)
        .min()
        .unwrap_or(0);
    let min_y = data
        .outputs
        .iter()
        .map(|output| output.origin.y)
        .min()
        .unwrap_or(0);
    let max_x = data
        .outputs
        .iter()
        .map(|output| output.origin.x.saturating_add(output.width as i32))
        .max()
        .unwrap_or(0);
    let max_y = data
        .outputs
        .iter()
        .map(|output| output.origin.y.saturating_add(output.height as i32))
        .max()
        .unwrap_or(0);
    let Ok(width) = u32::try_from(max_x.saturating_sub(min_x)) else {
        return;
    };
    let Ok(height) = u32::try_from(max_y.saturating_sub(min_y)) else {
        return;
    };
    let Some(byte_len) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return;
    };
    let mut desktop = vec![0_u8; byte_len];
    for output in &data.outputs {
        let Some((source_width, source_height, pixels)) =
            data.screenshot_all_captures.get(&output.output_id)
        else {
            return;
        };
        let dst_x = output.origin.x.saturating_sub(min_x) as usize;
        let dst_y = output.origin.y.saturating_sub(min_y) as usize;
        for row in 0..*source_height as usize {
            let src_start = row * *source_width as usize * 4;
            let src_end = src_start + *source_width as usize * 4;
            let dst_start = ((dst_y + row) * width as usize + dst_x) * 4;
            let dst_end = dst_start + *source_width as usize * 4;
            if src_end > pixels.len() || dst_end > desktop.len() {
                flog_warn!("Vulkan all-output screenshot layout exceeded its canvas");
                return;
            }
            desktop[dst_start..dst_end].copy_from_slice(&pixels[src_start..src_end]);
        }
    }
    data.desktop.state.screenshot_seq += 1;
    match crate::core::screenshot::save_srgb_rgba8_screenshot(
        width,
        height,
        &desktop,
        "all-outputs",
        data.desktop.state.screenshot_seq,
    ) {
        Ok(path) => flog(format!(
            "All-outputs screenshot saved to {}",
            path.display()
        )),
        Err(error) => flog_warn!("Vulkan all-output screenshot failed: {error:#}"),
    }
    data.desktop.state.screenshot_all_requested = false;
    data.screenshot_all_captures.clear();
}

fn finish_vulkan_capture(data: &mut VulkanDrmData, capture: AshDrmCapture) {
    let output_id = OutputId(capture.id);
    data.capture_pending.remove(&output_id);
    let width = capture.width;
    let height = capture.height;
    let rgba = match capture.into_rgba8() {
        Ok(pixels) => pixels,
        Err(error) => {
            flog_warn!("Vulkan capture conversion failed: {error:#}");
            return;
        }
    };
    if data.desktop.state.screenshot_request() == Some(output_id) {
        save_vulkan_screenshot(data, output_id, width, height, &rgba);
        data.desktop.state.clear_screenshot_request(output_id);
    }
    crate::core::portal::complete_pending_portal_captures_from_rgba(
        &mut data.desktop.state,
        output_id,
        width,
        height,
        &rgba,
    );
    crate::core::remote::export_rgba_frame(
        &mut data.desktop.state,
        output_id,
        width,
        height,
        &rgba,
    );
    if data.desktop.state.screenshot_all_requested {
        data.screenshot_all_captures
            .insert(output_id, (width, height, rgba));
        finish_all_outputs_screenshot(data);
    }
}

fn recover_vulkan_renderer(data: &mut VulkanDrmData, reason: &anyhow::Error) -> Result<()> {
    flog_warn!("raw Vulkan renderer failed; recreating device and scanout: {reason:#}");
    data.renderer.abandon_device();
    let replacement = AshDrmRenderer::new(data.primary_node.major(), data.primary_node.minor())
        .context("recreate raw Vulkan device")?;
    let abandoned = std::mem::replace(&mut data.renderer, replacement);
    drop(abandoned);
    match rebuild_vulkan_outputs(data)? {
        true => {
            data.desktop.state.mark_redraw();
            flog("raw Vulkan device and KMS scanout recovered in-process");
            Ok(())
        }
        false => Err(anyhow!(
            "cannot recover Vulkan renderer without an enabled connected output"
        )),
    }
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
    desktop.state.cpu_output_capture_available = true;
    let output_configs = select_outputs(&drm)?;
    let outputs =
        initialize_vulkan_outputs(&mut desktop, &renderer, &mut drm, &fd, output_configs)?;
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
    #[cfg(feature = "xwayland")]
    let xwayland_event_loop = EventLoop::try_new()?;

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|error| anyhow!("assign libinput seat: {error:?}"))?;
    let input_backend = LibinputInputBackend::new(libinput.clone());
    let mut input_session = session.clone();
    let udev = UdevBackend::new(session.seat())
        .map_err(|error| anyhow!("initialize udev backend: {error}"))?;
    let mut data = VulkanDrmData {
        desktop,
        #[cfg(feature = "xwayland")]
        xwayland_event_loop,
        renderer,
        scene_builder: VulkanSceneBuilder::default(),
        drm,
        drm_fd: fd,
        primary_node: node,
        session: session.clone(),
        outputs,
        libinput,
        session_active: session.is_active(),
        resume_pending: false,
        resume_retry_at: None,
        topology_refresh_pending: false,
        capture_pending: HashSet::new(),
        screenshot_all_captures: HashMap::new(),
        fatal_error: None,
        surface_blocker_loop,
    };

    #[cfg(feature = "xwayland")]
    {
        start_xwayland(
            &mut data.desktop.state,
            &data.desktop.display.handle(),
            data.xwayland_event_loop.handle(),
        )?;
        finish_xwayland_startup(
            &mut data.xwayland_event_loop,
            &mut data.desktop.display,
            &mut data.desktop.state,
            Duration::from_secs(30),
        )?;
        if let Some(display) = data.desktop.state.xwayland_display.as_deref() {
            flog(format!(
                "Raw Vulkan DRM: XWayland active on DISPLAY={display}"
            ));
        }
    }

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
                        if let Err(error) = input_session.change_vt(vt) {
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
                Ok(_) => {
                    output.frame_pending = false;
                    output.present_started_at = None;
                }
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
                pause_vulkan_session(data, "libseat PauseSession");
            }
            SessionEvent::ActivateSession => {
                data.resume_pending = true;
                data.resume_retry_at = Some(Instant::now() + Duration::from_millis(250));
                flog_warn!("raw Vulkan DRM ownership restored; scheduling settled resume");
            }
        })?;

    let _udev_token = loop_handle.insert_source(udev, |event, _, data| {
        let device_id = match event {
            UdevEvent::Added { device_id, .. }
            | UdevEvent::Changed { device_id }
            | UdevEvent::Removed { device_id } => device_id,
        };
        let Ok(node) = DrmNode::from_dev_id(device_id) else {
            return;
        };
        if node == data.primary_node {
            data.topology_refresh_pending = true;
            flog(format!(
                "raw Vulkan DRM connector event on {node:?}; scheduling topology rebuild"
            ));
        }
    })?;

    let sleep_notifications = spawn_session_sleep_watch().ok();

    flog_warn!(
        "Raw Vulkan DRM ready: outputs={} renderer={} (no Vulkan display WSI)",
        data.outputs.len(),
        data.renderer.info().adapter_name
    );
    while data.desktop.state.running {
        if let Some(rx) = sleep_notifications.as_ref() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    SessionSleepEvent::GoingToSleep => {
                        pause_vulkan_session(&mut data, "login1 PrepareForSleep(true)");
                    }
                    SessionSleepEvent::WokeUp => {
                        data.resume_pending = true;
                        data.resume_retry_at = Some(Instant::now() + Duration::from_millis(250));
                    }
                }
            }
        }
        if data.resume_pending
            && data
                .resume_retry_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if let Err(error) = resume_vulkan_session(&mut data, "settled resume") {
                flog_warn!("raw Vulkan DRM resume deferred: {error:#}");
                data.resume_retry_at = Some(Instant::now() + Duration::from_millis(500));
            }
        }

        #[cfg(feature = "xwayland")]
        data.xwayland_event_loop
            .dispatch(Some(Duration::ZERO), &mut data.desktop.state)?;

        pump_desktop_services(&mut data.desktop.state);
        if data.desktop.state.take_display_reconfigure_request() {
            data.topology_refresh_pending = true;
            flog("raw Vulkan DRM display settings changed; scheduling topology rebuild");
        }

        event_loop.dispatch(Some(FRAME_INTERVAL), &mut data)?;
        if data.topology_refresh_pending && data.session_active && !data.resume_pending {
            data.topology_refresh_pending = false;
            match rebuild_vulkan_outputs(&mut data) {
                Ok(true) => flog("raw Vulkan DRM topology rebuild complete"),
                Ok(false) => {}
                Err(error) => {
                    data.fatal_error = Some(error.context(
                        "raw Vulkan DRM topology rebuild failed after retiring old scanout",
                    ));
                }
            }
        }
        data.surface_blocker_loop
            .dispatch(Some(Duration::ZERO), &mut data.desktop.state)?;
        data.desktop.state.process_hdr_safe_session_action();
        data.desktop.state.process_deferred_ui_and_launches();
        if let Err(error) = data.renderer.poll() {
            recover_vulkan_renderer(&mut data, &error)?;
            continue;
        }
        while let Some(capture) = data.renderer.take_completed_capture() {
            finish_vulkan_capture(&mut data, capture);
        }
        if let Some(output_name) = data.outputs.iter().find_map(|output| {
            (output.frame_pending
                && output
                    .present_started_at
                    .is_some_and(|started| started.elapsed() >= KMS_PRESENT_TIMEOUT))
            .then(|| output.name.clone())
        }) {
            flog_warn!(
                "Vulkan KMS present on {} exceeded {} ms; rebuilding scanout",
                output_name,
                KMS_PRESENT_TIMEOUT.as_millis()
            );
            if !rebuild_vulkan_outputs(&mut data)? {
                return Err(anyhow!(
                    "cannot recover stalled Vulkan KMS present without a connected output"
                )
                .into());
            }
            continue;
        }
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
            crate::core::wayland::color_management_protocol::flush_pending_image_description_info_done(
                &mut data.desktop.state,
            );
        }
        data.desktop.state.process_deferred_window_ops();
        data.desktop.state.refresh_space();
        data.desktop.state.tick_layout();
        crate::core::portal::remove_dead_portal_sessions(&mut data.desktop.state);
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
            let capture_requested = (data.desktop.state.screenshot_request() == Some(output_id)
                || data.desktop.state.screenshot_all_requested
                || data
                    .desktop
                    .state
                    .pending_portal_captures
                    .iter()
                    .any(|capture| capture.output_id == output_id)
                || data
                    .desktop
                    .state
                    .output_capture_broker
                    .has_consumers_for_output(output_id))
                && !data.capture_pending.contains(&output_id);
            let sdr_output_matrix = data
                .desktop
                .state
                .outputs
                .get(&output_id)
                .map(|output| {
                    crate::core::color::scene_to_output_matrix(
                        output.color_description,
                        crate::core::color::RenderingIntent::Relative,
                    )
                })
                .unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
            let output_lut = data
                .desktop
                .state
                .outputs
                .get(&output_id)
                .and_then(|output| output.output_icc_lut.as_ref())
                .filter(|_| crate::core::icc_lut::icc_lut_shader_enabled())
                .map(|lut| AshDrmOutputLut {
                    grid_size: lut.grid_size,
                    rgb: &lut.rgb,
                });
            let output_transfer = data
                .desktop
                .state
                .outputs
                .get(&output_id)
                .map(|output| output.color_description.transfer)
                .filter(|transfer| {
                    matches!(
                        transfer,
                        crate::core::color::TransferFunction::Srgb
                            | crate::core::color::TransferFunction::Bt1886
                            | crate::core::color::TransferFunction::Gamma22
                            | crate::core::color::TransferFunction::Linear
                    )
                })
                .map(|transfer| match transfer {
                    crate::core::color::TransferFunction::Gamma22 => AshDrmTransfer::Gamma22,
                    _ => AshDrmTransfer::Srgb,
                })
                .unwrap_or_default();
            let visible_source_peak = scene
                .surfaces
                .iter()
                .map(|surface| surface.color_transform.source_peak_nits)
                .filter(|peak| peak.is_finite() && *peak > 0.0)
                .reduce(f32::max);
            let hdr_output = data
                .desktop
                .state
                .outputs
                .get(&output_id)
                .and_then(|output| {
                    let active =
                        output.hdr_requested && output.hdr_supported && output.hdr_kms_applied;
                    active.then(|| {
                        let appearance = output.hdr_appearance.validate().unwrap_or_default();
                        let (scene_to_bt2020, _, bt2020_luma) =
                            crate::core::color::hdr10_pq_encode_transforms(
                                output.color_description.primaries,
                            );
                        AshDrmHdrOutput {
                            peak_nits: appearance.peak_nits,
                            full_frame_peak_nits: appearance.full_frame_peak_nits,
                            black_level_nits: appearance.black_level_nits,
                            reference_white_nits: appearance.reference_white_nits,
                            source_peak_nits: visible_source_peak.unwrap_or(appearance.peak_nits),
                            saturation: appearance.saturation,
                            midtone_gamma: appearance.midtone_gamma,
                            calibration_pattern: output.hdr_calibration_pattern.shader_value(),
                            scene_to_bt2020,
                            bt2020_luma,
                        }
                    })
                });
            let output_matrix = if hdr_output.is_some() {
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            } else {
                sdr_output_matrix
            };
            let output_transfer = if hdr_output.is_some() {
                AshDrmTransfer::Pq
            } else if HDR_SCANOUT_FORMATS.contains(&data.outputs[index].scanout.format()) {
                match output_transfer {
                    AshDrmTransfer::Gamma22 => AshDrmTransfer::Gamma22Unorm,
                    _ => AshDrmTransfer::SrgbUnorm,
                }
            } else {
                output_transfer
            };
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
                &scene.foreground,
                scene.foreground_after_surface,
                &scene.egui_textures,
                &scene.egui_meshes,
                scene.egui_before_surface,
                &scene.damage,
                output_id.0,
                capture_requested.then_some(output_id.0),
                output_matrix,
                output_transfer,
                output_lut,
                hdr_output,
            )?;
            if capture_requested {
                data.capture_pending.insert(output_id);
            }
            let sync = SyncPoint::from(KmsFence(submission.fence_fd));
            let kms_damage = scene
                .damage
                .iter()
                .filter(|[_, _, width, height]| *width > 0 && *height > 0)
                .map(|[x, y, width, height]| {
                    Rectangle::from_loc_and_size((*x, *y), (*width, *height))
                })
                .collect::<Vec<_>>();
            let kms_damage = (!kms_damage.is_empty()).then_some(kms_damage);
            match output.scanout.queue_buffer(Some(sync), kms_damage, ()) {
                Ok(()) => {
                    output.frame_pending = true;
                    output.present_started_at = Some(Instant::now());
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

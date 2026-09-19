//! Raw Vulkan DRM backend: Smithay/libseat own KMS, GBM owns scanout, and
//! ash only renders into DMA-BUFs and exports explicit fences.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::hash::{DefaultHasher, Hash, Hasher};
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
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, GbmBufferedSurface, PlaneClaim,
};
use smithay::backend::input::{
    InputEvent, KeyState, KeyboardKeyEvent, SwitchState, SwitchToggleEvent,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::sync::{Fence, Interrupted, SyncPoint};
use smithay::backend::session::{libseat::LibSeatSession, Event as SessionEvent, Session};
use smithay::backend::udev::{primary_gpu, UdevBackend, UdevEvent};
use smithay::output::{Mode as WlMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::drm::control::{connector, crtc, dumbbuffer, plane, Device as _, Mode};
use smithay::reexports::drm::{buffer::Buffer as _, Device as _, DriverCapability};
use smithay::reexports::input::event::switch::Switch as InputSwitch;
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState};

use super::common::{
    bootstrap_compositor_core, client_state_from_stream, is_nonfatal_wayland_io_error,
    physical_size_mm_from_pixels, pump_desktop_services, restart_shell_surfaces_after_gpu_resume,
    spawn_session_sleep_watch, stop_focaldesk_session_target, NestedDesktop, SessionSleepEvent,
};
#[cfg(feature = "xwayland")]
use super::common::{finish_xwayland_startup, start_xwayland};
use super::drm::{
    connector_edid, disable_explicit_kms_fences, dispatch_backend_input_event,
    display_matches_monitor, drm_card_vendor_id, hdr_appearance_from_support, hdr_detection,
    load_display_config, merge_disconnected_display_configs, parse_edid_identity,
    select_connector_mode, write_display_config, DisplayConfig, DisplayModeConfig, HdrSupport,
};
use super::wgpu_nested::{VulkanCompositorScene, VulkanSceneBuilder};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const KMS_PRESENT_TIMEOUT: Duration = Duration::from_secs(5);
const HDR_STATE_VALIDATION_INTERVAL: Duration = Duration::from_secs(2);
const HDR_REARM_VERIFY_TIMEOUT: Duration = Duration::from_secs(1);
const HDR_REARM_ATTEMPTS_BEFORE_REBUILD: u8 = 3;
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

impl KmsFence {
    fn wait_timeout(&self, timeout: Duration) -> std::io::Result<bool> {
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        loop {
            let mut pollfd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if result > 0 {
                return Ok(true);
            }
            if result == 0 {
                return Ok(false);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

struct OutputConfig {
    name: String,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    available_modes: Vec<DisplayModeConfig>,
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
    connector: connector::Handle,
    width: u32,
    height: u32,
    output_id: OutputId,
    origin: Point<i32, Logical>,
    crtc: crtc::Handle,
    scanout: VulkanScanout,
    frame_pending: bool,
    present_started_at: Option<Instant>,
    hdr_metadata_blob: Option<u64>,
    hdr_support: HdrSupport,
    next_hdr_validation: Instant,
    hdr_last_validation_ok: bool,
    hdr_rearm_pending: bool,
    hdr_rearm_attempts: u8,
    hdr_transition_retry_at: Option<Instant>,
    hdr_source_peak_initialized: bool,
    last_hdr_source_peak_bits: Option<u32>,
    cursor: Option<KmsCursor>,
}

fn hdr_source_peak_changed(initialized: bool, previous: Option<u32>, current: Option<u32>) -> bool {
    initialized && previous != current
}

fn expand_dmabuf_damage(scene: &mut VulkanCompositorScene) {
    // The raw renderer samples client DMA-BUFs directly into a retained linear
    // scene image. Streaming clients can update a reused allocation without
    // consistently advancing Wayland damage, so repaint every visible imported
    // quad whenever a frame is already scheduled. This stays bounded to DMA-BUF
    // surfaces rather than widening the update to the whole output.
    let dmabufs = scene
        .surfaces
        .iter()
        .filter(|surface| surface.dmabuf.is_some())
        .map(|surface| surface.destination)
        .filter(|[_, _, width, height]| *width > 0 && *height > 0)
        .collect::<Vec<_>>();
    for rect in dmabufs {
        if !scene.damage.contains(&rect) {
            scene.damage.push(rect);
        }
    }
}

struct KmsCursor {
    fd: DrmDeviceFd,
    _claim: PlaneClaim,
    buffer: Option<dumbbuffer::DumbBuffer>,
    size: (u32, u32),
    uploaded_key: Option<u64>,
    last_location: Option<(i32, i32)>,
    enabled: bool,
    failure_logged: bool,
    retry_after: Option<Instant>,
}

impl Drop for KmsCursor {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            let _ = self.fd.destroy_dumb_buffer(buffer);
        }
    }
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
    disable_explicit_kms_fences: bool,
    resume_pending: bool,
    resume_retry_at: Option<Instant>,
    restart_shell_after_present: bool,
    topology_refresh_pending: bool,
    hdr_recovery_rebuild_attempted: bool,
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
    data.restart_shell_after_present = false;
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
    // A Vulkan device that survives suspend can still return apparently valid
    // sync-file fences which NVIDIA can no longer import into KMS.  Reusing
    // that device leaves the first post-resume frame pending forever and a
    // retained pre-suspend frame on screen (most visibly: lock chrome without
    // its text).  Recreate both the Vulkan device and scanout buffers while
    // libseat ownership is active so all post-resume fences share the driver's
    // new synchronization epoch.
    recover_vulkan_renderer(
        data,
        &anyhow!("session resumed; GPU synchronization state is stale"),
    )
    .context("recreate raw Vulkan renderer after resume")?;
    data.libinput
        .resume()
        .map_err(|()| anyhow!("resume libinput"))?;
    data.session_active = true;
    data.resume_pending = false;
    data.resume_retry_at = None;
    // GTK uses a separate GPU context. Recreate those clients only after a
    // vblank proves the replacement Vulkan device and KMS scanout are live;
    // otherwise a rail that retained invalid driver resources can remain
    // visible but stop accepting input for the rest of the session.
    data.restart_shell_after_present = true;
    data.topology_refresh_pending = false;
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
        let edid = connector_edid(drm, *handle);
        let hdr_support = hdr_detection::connector_hdr_support(drm, *handle, edid.as_deref());
        let identity = edid.as_deref().and_then(parse_edid_identity);
        let saved_monitor = configured
            .iter()
            .find(|display| display_matches_monitor(display, identity.as_ref()));
        if saved_monitor.is_some_and(|display| !display.enabled) {
            flog(format!(
                "Raw Vulkan DRM leaving configured-disabled output {name} off"
            ));
            continue;
        }
        let requested_mode = saved_monitor
            .map(|display| (display.mode_width, display.mode_height, display.refresh_mhz));
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
        let hdr_requested = saved_monitor
            .map(|display| display.hdr_requested || display.hdr_enabled)
            .unwrap_or(false);
        let hdr_appearance = saved_monitor
            .map(|display| display.hdr_appearance)
            .and_then(|appearance| appearance.validate().ok())
            .unwrap_or_else(|| hdr_appearance_from_support(&hdr_support));
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
        let scale = saved_monitor
            .map(|display| display.scale)
            .filter(|scale| scale.is_finite() && (1.0..=4.0).contains(scale))
            .unwrap_or(1.0);
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
            available_modes: info
                .modes()
                .iter()
                .map(|candidate| {
                    let (width, height) = candidate.size();
                    DisplayModeConfig {
                        width: i32::from(width),
                        height: i32::from(height),
                        refresh_mhz: (candidate.vrefresh() as i32).max(1) * 1_000,
                    }
                })
                .collect(),
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
            color_profile: saved_monitor
                .map(|display| display.color_profile)
                .unwrap_or_default(),
            icc_profile_path: saved_monitor.and_then(|display| display.icc_profile_path.clone()),
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
        let snapshots = outputs
            .iter()
            .map(|output| {
                let saved = configured
                    .iter()
                    .find(|display| display.name == output.name);
                let metadata = output.hdr_support.edid_hdr_metadata;
                DisplayConfig {
                    name: output.name.clone(),
                    monitor_make: Some(output.make.clone()),
                    monitor_model: Some(output.model.clone()),
                    monitor_serial: Some(output.serial_number.clone()),
                    enabled: true,
                    mode_width: output.width as i32,
                    mode_height: output.height as i32,
                    refresh_mhz: (output.mode.vrefresh() as i32).max(1) * 1_000,
                    available_modes: output.available_modes.clone(),
                    scale: output.scale,
                    logical_x: output.origin.x,
                    logical_y: output.origin.y,
                    physical_width_mm: Some(output.physical_size_mm.0),
                    physical_height_mm: Some(output.physical_size_mm.1),
                    primary: output.primary,
                    transform: saved
                        .map(|display| display.transform.clone())
                        .unwrap_or(super::drm::DisplayTransform::Normal),
                    hdr_supported: output.hdr_support.is_detected(),
                    hdr_max_luminance_nits: metadata.map(|value| f32::from(value.max_luminance)),
                    hdr_max_fall_nits: metadata.map(|value| f32::from(value.max_fall)),
                    hdr_requested: output.hdr_requested,
                    hdr_enabled: false,
                    hdr_appearance: output.hdr_appearance,
                    color_profile: output.color_profile,
                    icc_profile_path: output.icc_profile_path.clone(),
                }
            })
            .collect::<Vec<_>>();
        let snapshots = merge_disconnected_display_configs(snapshots, &configured);
        if let Err(error) = write_display_config(&snapshots) {
            flog_warn!("Raw Vulkan could not refresh display inventory: {error:#}");
        }
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

fn create_kms_cursor(
    drm: &DrmDevice,
    fd: &DrmDeviceFd,
    scanout: &VulkanScanout,
) -> Result<Option<KmsCursor>> {
    let Some(info) = scanout.surface().planes().cursor.iter().find(|info| {
        info.formats
            .iter()
            .any(|format| format.code == Fourcc::Argb8888)
    }) else {
        return Ok(None);
    };
    let Some(claim) = scanout.surface().claim_plane(info.handle) else {
        return Ok(None);
    };
    let size = drm.cursor_size();
    if size.w == 0 || size.h == 0 {
        return Ok(None);
    }
    let buffer = fd
        .create_dumb_buffer((size.w, size.h), Fourcc::Argb8888, 32)
        .context("allocate KMS cursor buffer")?;
    Ok(Some(KmsCursor {
        fd: fd.clone(),
        _claim: claim,
        buffer: Some(buffer),
        size: (size.w, size.h),
        uploaded_key: None,
        last_location: None,
        enabled: false,
        failure_logged: false,
        retry_after: None,
    }))
}

fn cursor_image_key(icon: focaldesk_cursor::CursorIcon, width: u32, height: u32) -> u64 {
    let mut hash = DefaultHasher::new();
    icon.hash(&mut hash);
    width.hash(&mut hash);
    height.hash(&mut hash);
    hash.finish()
}

fn upload_kms_cursor(cursor: &mut KmsCursor, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    let (buffer_width, buffer_height) = cursor.size;
    if width > buffer_width || height > buffer_height {
        return Err(anyhow!(
            "cursor image {width}x{height} exceeds KMS plane {buffer_width}x{buffer_height}"
        ));
    }
    let buffer = cursor
        .buffer
        .as_mut()
        .context("KMS cursor buffer missing")?;
    let pitch = buffer.pitch() as usize;
    let mut mapping = cursor
        .fd
        .map_dumb_buffer(buffer)
        .context("map KMS cursor buffer")?;
    copy_cursor_rgba_to_argb(mapping.as_mut(), pitch, rgba, width, height)?;
    Ok(())
}

fn copy_cursor_rgba_to_argb(
    destination: &mut [u8],
    pitch: usize,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let row_bytes = width as usize * 4;
    let required_source = row_bytes * height as usize;
    let required_destination = pitch * height as usize;
    if rgba.len() < required_source || destination.len() < required_destination {
        return Err(anyhow!("cursor pixel buffer is truncated"));
    }
    destination.fill(0);
    for row in 0..height as usize {
        let source = &rgba[row * row_bytes..(row + 1) * row_bytes];
        let row_destination = &mut destination[row * pitch..row * pitch + row_bytes];
        for (src, dst) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(row_destination.as_chunks_mut::<4>().0)
        {
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
    }
    Ok(())
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

fn update_kms_cursor(data: &mut VulkanDrmData) {
    let visible = data.desktop.state.cursor_manager.visible();
    let custom_surface = data.desktop.state.render.sw_cursor_surface.is_some();
    let (pointer_x, pointer_y) = data.desktop.state.cursor_manager.position();
    let icon = data.desktop.state.cursor_manager.current_flow_icon();
    let image = if visible && !custom_surface {
        data.desktop
            .state
            .cursor_manager
            .current_image()
            .ok()
            .map(|image| {
                (
                    image.width,
                    image.height,
                    image.hotspot_x,
                    image.hotspot_y,
                    image.rgba.clone(),
                )
            })
    } else {
        None
    };
    let mut hardware_active = false;
    for output in &mut data.outputs {
        let owns_pointer = data.desktop.state.output_contains_pointer(output.output_id);
        let Some(cursor) = output.cursor.as_mut() else {
            continue;
        };
        // The legacy cursor ioctls below still become atomic plane commits on
        // atomic-KMS drivers.  Issuing one while the primary-plane page flip is
        // outstanding races that commit and commonly returns EBUSY.  Keep the
        // last accepted cursor visible and apply the newest pointer state from
        // the vblank handler once the primary commit has retired.  Pointer
        // motion is naturally coalesced because CursorManager stores only its
        // latest position.
        if output.frame_pending {
            hardware_active |= owns_pointer && cursor.enabled;
            continue;
        }
        if cursor
            .retry_after
            .is_some_and(|retry| Instant::now() < retry)
        {
            hardware_active |= owns_pointer && cursor.enabled;
            continue;
        }
        if !visible || custom_surface || !owns_pointer {
            if cursor.enabled {
                #[allow(deprecated)]
                let cleared = cursor
                    .fd
                    .set_cursor(output.crtc, Option::<&dumbbuffer::DumbBuffer>::None);
                if cleared.is_ok() {
                    cursor.enabled = false;
                    cursor.last_location = None;
                    cursor.retry_after = None;
                }
            }
            continue;
        }
        let Some((width, height, hotspot_x, hotspot_y, ref rgba)) = image else {
            continue;
        };
        let Some(output_state) = data.desktop.state.outputs.get(&output.output_id) else {
            continue;
        };
        let key = cursor_image_key(icon, width, height);
        if cursor.uploaded_key != Some(key) {
            if let Err(error) = upload_kms_cursor(cursor, rgba, width, height) {
                cursor.retry_after = Some(Instant::now() + Duration::from_millis(250));
                if !cursor.failure_logged {
                    flog_warn!("KMS cursor upload failed on {}: {error:#}", output.name);
                    cursor.failure_logged = true;
                }
                continue;
            }
            cursor.uploaded_key = Some(key);
        }
        let x = ((pointer_x - f64::from(output_state.logical_origin.x)) * output_state.scale_factor)
            .round() as i32
            - hotspot_x as i32;
        let y = ((pointer_y - f64::from(output_state.logical_origin.y)) * output_state.scale_factor)
            .round() as i32
            - hotspot_y as i32;
        if cursor.enabled
            && cursor.uploaded_key == Some(key)
            && cursor.last_location == Some((x, y))
        {
            hardware_active = true;
            continue;
        }
        let first_enable = !cursor.enabled;
        #[allow(deprecated)]
        let result = (|| {
            if first_enable {
                let buffer = cursor
                    .buffer
                    .as_ref()
                    .context("KMS cursor buffer missing")?;
                cursor
                    .fd
                    .set_cursor(output.crtc, Some(buffer))
                    .context("enable KMS cursor")?;
            }
            cursor
                .fd
                .move_cursor(output.crtc, (x, y))
                .context("move KMS cursor")
        })();
        match result {
            Ok(()) => {
                cursor.enabled = true;
                cursor.last_location = Some((x, y));
                cursor.failure_logged = false;
                cursor.retry_after = None;
                hardware_active = true;
                if first_enable {
                    flog(format!("KMS hardware cursor active on {}", output.name));
                }
            }
            Err(error) => {
                let busy = error
                    .chain()
                    .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                    .any(|cause| cause.raw_os_error() == Some(libc::EBUSY));
                cursor.retry_after = Some(
                    Instant::now()
                        + if busy {
                            FRAME_INTERVAL
                        } else {
                            Duration::from_millis(250)
                        },
                );
                if !cursor.failure_logged {
                    flog_warn!(
                        "KMS cursor move failed on {}; using software fallback: {error}",
                        output.name
                    );
                    cursor.failure_logged = true;
                }
            }
        }
    }
    data.desktop
        .state
        .cursor_manager
        .set_hardware_cursor_ready(hardware_active);
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
        let cursor = match create_kms_cursor(drm, fd, &scanout) {
            Ok(cursor) => cursor,
            Err(error) => {
                flog_warn!("KMS cursor unavailable on {}: {error:#}", config.name);
                None
            }
        };

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
            connector: config.connector,
            width: config.width,
            height: config.height,
            output_id: config.output_id,
            origin: config.origin,
            crtc: config.crtc,
            scanout,
            frame_pending: false,
            present_started_at: None,
            hdr_metadata_blob,
            hdr_support: config.hdr_support,
            next_hdr_validation: Instant::now() + HDR_STATE_VALIDATION_INTERVAL,
            hdr_last_validation_ok: false,
            hdr_rearm_pending: false,
            hdr_rearm_attempts: 0,
            hdr_transition_retry_at: None,
            hdr_source_peak_initialized: false,
            last_hdr_source_peak_bits: None,
            cursor,
        });
    }
    desktop
        .state
        .cursor_manager
        .set_hardware_cursor_ready(false);
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
        data.renderer.remove_stream(output.output_id.0);
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

/// A sink-side picture-mode change or link retrain can leave scanout running
/// while silently dropping connector colorspace or HDR metadata. Periodically
/// verify the live properties and re-arm them with the next atomic frame.
/// Rendering stays PQ while repair is pending, preventing an SDR frame from
/// being committed at the same time as restored HDR signaling.
fn maintain_vulkan_hdr_state(data: &mut VulkanDrmData) {
    let now = Instant::now();
    for index in 0..data.outputs.len() {
        let output_id = data.outputs[index].output_id;
        let hdr_expected = data
            .desktop
            .state
            .outputs
            .get(&output_id)
            .is_some_and(|output| {
                output.hdr_requested && output.hdr_supported && output.hdr_kms_applied
            });
        if !hdr_expected || data.outputs[index].hdr_metadata_blob.is_none() {
            data.outputs[index].hdr_rearm_pending = false;
            data.outputs[index].hdr_rearm_attempts = 0;
            data.outputs[index].next_hdr_validation = now + HDR_STATE_VALIDATION_INTERVAL;
            continue;
        }
        if data.outputs[index].frame_pending || now < data.outputs[index].next_hdr_validation {
            continue;
        }

        let connector = data.outputs[index].connector;
        let require_max_bpc = data.outputs[index].hdr_support.max_bpc.is_some();
        match hdr_detection::hdr_kms::validate_connector_hdr_state(
            &data.drm,
            connector,
            true,
            require_max_bpc,
        ) {
            Ok(_) => {
                data.outputs[index].hdr_last_validation_ok = true;
                if data.outputs[index].hdr_rearm_pending {
                    flog_warn!(
                        "Raw Vulkan HDR signaling recovered on {} after {} re-arm attempt(s)",
                        data.outputs[index].name,
                        data.outputs[index].hdr_rearm_attempts
                    );
                }
                data.outputs[index].hdr_rearm_pending = false;
                data.outputs[index].hdr_rearm_attempts = 0;
                data.outputs[index].next_hdr_validation = now + HDR_STATE_VALIDATION_INTERVAL;
            }
            Err(readback_error) => {
                data.outputs[index].hdr_last_validation_ok = false;
                let attempts = data.outputs[index].hdr_rearm_attempts;
                if attempts >= HDR_REARM_ATTEMPTS_BEFORE_REBUILD {
                    if data.hdr_recovery_rebuild_attempted {
                        flog_warn!(
                            "Raw Vulkan HDR signaling remains invalid on {} after a recovery rebuild ({readback_error:#}); retaining PQ scanout and continuing bounded re-arm attempts",
                            data.outputs[index].name
                        );
                    } else {
                        flog_warn!(
                            "Raw Vulkan HDR signaling remained invalid on {} after {} re-arm attempts ({readback_error:#}); scheduling one output rebuild",
                            data.outputs[index].name,
                            attempts
                        );
                        data.hdr_recovery_rebuild_attempted = true;
                        data.topology_refresh_pending = true;
                    }
                    data.outputs[index].hdr_rearm_pending = false;
                    data.outputs[index].hdr_rearm_attempts = 0;
                    data.outputs[index].next_hdr_validation = now + HDR_STATE_VALIDATION_INTERVAL;
                    continue;
                }

                let metadata_blob = data.outputs[index]
                    .hdr_metadata_blob
                    .expect("HDR expectation requires a metadata blob");
                let staged = hdr_detection::hdr_kms::build_connector_hdr_state(
                    &data.drm,
                    connector,
                    &data.outputs[index].hdr_support,
                    true,
                    Some(metadata_blob),
                )
                .and_then(|state| {
                    state.ok_or_else(|| anyhow!("connector exposes no programmable HDR state"))
                })
                .and_then(|state| {
                    data.outputs[index]
                        .scanout
                        .surface()
                        .use_hdr_state(state)
                        .map_err(|error| anyhow!("queue HDR connector re-arm: {error}"))
                });

                data.outputs[index].hdr_rearm_pending = true;
                data.outputs[index].hdr_rearm_attempts = attempts.saturating_add(1);
                data.outputs[index].next_hdr_validation = now + HDR_REARM_VERIFY_TIMEOUT;
                match staged {
                    Ok(()) => {
                        flog_warn!(
                            "Raw Vulkan HDR signaling lost on {} ({readback_error:#}); staged atomic re-arm attempt {}",
                            data.outputs[index].name,
                            data.outputs[index].hdr_rearm_attempts
                        );
                        data.desktop.state.mark_output_full_damage(
                            output_id,
                            crate::core::desktop::DamageSource::Unknown,
                        );
                    }
                    Err(stage_error) => {
                        flog_warn!(
                            "Raw Vulkan HDR re-arm staging failed on {} after invalid readback ({readback_error:#}): {stage_error:#}",
                            data.outputs[index].name
                        );
                    }
                }
            }
        }
    }

    let active_hdr_outputs = data.outputs.iter().filter(|output| {
        data.desktop
            .state
            .outputs
            .get(&output.output_id)
            .is_some_and(|state| {
                state.hdr_requested && state.hdr_supported && state.hdr_kms_applied
            })
    });
    let mut active_count = 0;
    let all_active_valid = active_hdr_outputs.fold(true, |all_valid, output| {
        active_count += 1;
        all_valid && output.hdr_last_validation_ok
    });
    if active_count > 0 && all_active_valid {
        data.hdr_recovery_rebuild_attempted = false;
    }
}

/// Stage connector HDR changes requested by the desktop state.
///
/// Raw Vulkan used to program HDR only while creating an output. Clearing
/// `hdr_requested` for logout, suspend, restart, or shutdown therefore never
/// queued the matching SDR connector state, and the pending session action
/// waited forever for `hdr_kms_applied` to become false.
fn stage_vulkan_hdr_transitions(data: &mut VulkanDrmData) {
    for index in 0..data.outputs.len() {
        let output_id = data.outputs[index].output_id;
        let Some(state) = data.desktop.state.outputs.get(&output_id) else {
            continue;
        };
        if data.outputs[index].frame_pending || state.hdr_transition_target.is_some() {
            continue;
        }
        if data.outputs[index]
            .hdr_transition_retry_at
            .is_some_and(|retry_at| Instant::now() < retry_at)
        {
            continue;
        }

        let target = state.hdr_requested
            && state.hdr_supported
            && data.outputs[index].hdr_metadata_blob.is_some()
            && HDR_SCANOUT_FORMATS.contains(&data.outputs[index].scanout.format());
        if target == state.hdr_kms_applied {
            continue;
        }

        let output = &mut data.outputs[index];
        let connector_state = hdr_detection::hdr_kms::build_connector_hdr_state(
            &data.drm,
            output.connector,
            &output.hdr_support,
            target,
            if target {
                output.hdr_metadata_blob
            } else {
                None
            },
        );
        let staged = connector_state.and_then(|connector_state| {
            let Some(connector_state) = connector_state else {
                return Err(anyhow!("connector exposes no programmable HDR state"));
            };
            output
                .scanout
                .surface()
                .use_hdr_state(connector_state)
                .map_err(|error| anyhow!("queue connector HDR transition: {error}"))
        });

        match staged {
            Ok(()) => {
                data.outputs[index].hdr_transition_retry_at = None;
                if let Some(state) = data.desktop.state.outputs.get_mut(&output_id) {
                    state.hdr_transition_target = Some(target);
                }
                data.desktop.state.mark_output_full_damage(
                    output_id,
                    crate::core::desktop::DamageSource::Unknown,
                );
                flog_warn!(
                    "Raw Vulkan HDR KMS transition staged on {}: target={target}",
                    data.outputs[index].name
                );
            }
            Err(error) => {
                data.outputs[index].hdr_transition_retry_at =
                    Some(Instant::now() + HDR_REARM_VERIFY_TIMEOUT);
                flog_warn!(
                    "Raw Vulkan HDR KMS transition staging failed on {}: {error:#}",
                    data.outputs[index].name
                );
            }
        }
    }
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
            data.desktop.state.clear_screenshot_request(output_id);
            if data.desktop.state.screenshot_all_requested {
                data.desktop.state.screenshot_all_requested = false;
                data.screenshot_all_captures.clear();
            }
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
    flog_warn!("recreating raw Vulkan device and scanout: {reason:#}");
    // The replacement renderer starts with an empty texture cache. Make egui
    // resend its retained font/image atlases instead of referring to cache keys
    // that only existed on the abandoned Vulkan device.
    data.desktop.state.invalidate_gpu_state();
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
    let gpu_vendor_id = drm_card_vendor_id(&primary_path);
    let disable_explicit_kms_fences = disable_explicit_kms_fences(gpu_vendor_id);
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
        disable_explicit_kms_fences,
        resume_pending: false,
        resume_retry_at: None,
        restart_shell_after_present: false,
        topology_refresh_pending: false,
        hdr_recovery_rebuild_attempted: false,
        capture_pending: HashSet::new(),
        screenshot_all_captures: HashMap::new(),
        fatal_error: None,
        surface_blocker_loop,
    };

    if data.disable_explicit_kms_fences {
        flog_warn!(
            "Raw Vulkan disabled explicit KMS input fences for NVIDIA; submissions wait before implicit-sync atomic commits"
        );
    }

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
        update_kms_cursor(data);
    })?;

    let _drm_token = loop_handle.insert_source(drm_notifier, |event, _, data| match event {
        DrmEvent::VBlank(crtc) => {
            let Some(index) = data.outputs.iter().position(|output| output.crtc == crtc) else {
                return;
            };
            let completion = {
                let output = &mut data.outputs[index];
                output.scanout.frame_submitted()
            };
            match completion {
                Ok(_) => {
                    let output = &mut data.outputs[index];
                    output.frame_pending = false;
                    output.present_started_at = None;
                    if let Some(state) = data.desktop.state.outputs.get_mut(&output.output_id) {
                        if let Some(target) = state.hdr_transition_target.take() {
                            state.hdr_kms_applied = target;
                            state.hdr_enabled = target;
                            if !target {
                                state.hdr_verification_pending = false;
                                output.hdr_last_validation_ok = false;
                                output.hdr_rearm_pending = false;
                                output.hdr_rearm_attempts = 0;
                            }
                            output.hdr_transition_retry_at = None;
                            flog_warn!(
                                "Raw Vulkan HDR KMS transition completed on {}: active={target}",
                                output.name
                            );
                        }
                    }
                    if output.hdr_rearm_pending {
                        output.next_hdr_validation = Instant::now();
                    }
                    // A cursor update may have been deferred while this atomic
                    // primary-plane commit was pending.  Submit the newest
                    // coalesced position now that the CRTC is idle.
                    update_kms_cursor(data);
                    if std::mem::take(&mut data.restart_shell_after_present) {
                        flog("raw Vulkan first post-resume page flip completed");
                        restart_shell_surfaces_after_gpu_resume();
                    }
                }
                Err(error) => {
                    let output = &data.outputs[index];
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
    'main: while data.desktop.state.running {
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
        stage_vulkan_hdr_transitions(&mut data);
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
                "Vulkan KMS present on {} exceeded {} ms; recreating renderer and scanout",
                output_name,
                KMS_PRESENT_TIMEOUT.as_millis()
            );
            let reason = anyhow!(
                "KMS present on {output_name} exceeded {} ms",
                KMS_PRESENT_TIMEOUT.as_millis()
            );
            recover_vulkan_renderer(&mut data, &reason)?;
            continue;
        }
        if let Some(error) = data.fatal_error.take() {
            return Err(error.into());
        }
        if !data.session_active {
            continue;
        }
        maintain_vulkan_hdr_state(&mut data);
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

            let software_cursor = data.desktop.state.cursor_manager.software_cursor_needed();
            let mut scene = data.scene_builder.build_for_output_with_cursor_policy(
                &mut data.desktop.state,
                output_id,
                software_cursor,
                false,
                true,
            );
            expand_dmabuf_damage(&mut scene);
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
            let hdr_source_peak_bits = hdr_output.map(|hdr| hdr.source_peak_nits.to_bits());
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
            let target = {
                let output = &mut data.outputs[index];
                if hdr_source_peak_changed(
                    output.hdr_source_peak_initialized,
                    output.last_hdr_source_peak_bits,
                    hdr_source_peak_bits,
                ) {
                    // Source peak selects one output-wide tone curve, so a change invalidates
                    // retained pixels outside the surface that introduced the new peak.
                    scene.damage = vec![[0, 0, output.width as i32, output.height as i32]];
                }
                let (dmabuf, _) = match output.scanout.next_buffer() {
                    Ok(next) => next,
                    Err(error) => {
                        flog_warn!("Vulkan GBM acquire skipped on {}: {error}", output.name);
                        continue;
                    }
                };
                target_from_dmabuf(&dmabuf)?
            };
            let submission = match data.renderer.render(
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
            ) {
                Ok(submission) => submission,
                Err(error) => {
                    data.desktop.state.mark_output_full_damage(
                        output_id,
                        crate::core::desktop::DamageSource::Unknown,
                    );
                    recover_vulkan_renderer(&mut data, &error)
                        .context("recover from raw Vulkan frame-build failure")?;
                    continue 'main;
                }
            };
            if capture_requested {
                data.capture_pending.insert(output_id);
            }
            let sync = if data.disable_explicit_kms_fences {
                let fence = KmsFence(submission.fence_fd);
                match fence.wait_timeout(KMS_PRESENT_TIMEOUT) {
                    Ok(true) => None,
                    Ok(false) => {
                        data.desktop.state.mark_output_full_damage(
                            output_id,
                            crate::core::desktop::DamageSource::Unknown,
                        );
                        let error = anyhow!(
                            "Vulkan submission for {} did not complete within {} ms",
                            data.outputs[index].name,
                            KMS_PRESENT_TIMEOUT.as_millis()
                        );
                        recover_vulkan_renderer(&mut data, &error)
                            .context("recover from NVIDIA implicit-sync wait timeout")?;
                        continue 'main;
                    }
                    Err(error) => {
                        data.desktop.state.mark_output_full_damage(
                            output_id,
                            crate::core::desktop::DamageSource::Unknown,
                        );
                        let error = anyhow!("wait for NVIDIA Vulkan submission: {error}");
                        recover_vulkan_renderer(&mut data, &error)
                            .context("recover from NVIDIA implicit-sync wait failure")?;
                        continue 'main;
                    }
                }
            } else {
                Some(SyncPoint::from(KmsFence(submission.fence_fd)))
            };
            let kms_damage = scene
                .damage
                .iter()
                .filter(|[_, _, width, height]| *width > 0 && *height > 0)
                .map(|[x, y, width, height]| {
                    Rectangle::from_loc_and_size((*x, *y), (*width, *height))
                })
                .collect::<Vec<_>>();
            let kms_damage = (!kms_damage.is_empty()).then_some(kms_damage);
            let output = &mut data.outputs[index];
            match output.scanout.queue_buffer(sync, kms_damage, ()) {
                Ok(()) => {
                    output.hdr_source_peak_initialized = true;
                    output.last_hdr_source_peak_bits = hdr_source_peak_bits;
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

#[cfg(test)]
mod tests {
    use super::{copy_cursor_rgba_to_argb, expand_dmabuf_damage, hdr_source_peak_changed};
    use crate::backend::wgpu_nested::VulkanCompositorScene;
    use focaldesk_render::{
        FramePixelFormat, FrameTransform, LinuxDmabuf, TextureColorTransform, TextureQuad,
    };
    use std::os::fd::OwnedFd;
    use std::sync::Arc;

    fn test_texture(dmabuf: bool, damage: Vec<[u32; 4]>, destination: [i32; 4]) -> TextureQuad {
        TextureQuad {
            cache_key: 1,
            pixels: Vec::new(),
            width: 100,
            height: 50,
            stride: 400,
            format: FramePixelFormat::Bgra8Srgb,
            color_transform: TextureColorTransform::default(),
            dmabuf: dmabuf.then(|| LinuxDmabuf {
                planes: vec![Arc::new(OwnedFd::from(
                    std::fs::File::open("/dev/null").unwrap(),
                ))],
                fourcc: 0,
                modifier: 0,
                offsets: vec![0],
                strides: vec![400],
            }),
            damage,
            destination,
            source_uv: [0.0, 0.0, 1.0, 1.0],
            transform: FrameTransform::Normal,
            tint: [1.0; 4],
            retention: None,
        }
    }

    fn test_scene(surfaces: Vec<TextureQuad>) -> VulkanCompositorScene {
        VulkanCompositorScene {
            background: Vec::new(),
            surfaces,
            overlay: Vec::new(),
            overlay_after_surface: 0,
            foreground: Vec::new(),
            foreground_after_surface: 0,
            egui_textures: Vec::new(),
            egui_meshes: Vec::new(),
            egui_before_surface: 0,
            damage: vec![[1, 2, 3, 4]],
            client_surface_count: 0,
            cursor_present: false,
        }
    }

    #[test]
    fn every_visible_dmabuf_expands_damage_to_its_whole_destination() {
        let mut scene = test_scene(vec![
            test_texture(true, vec![[0, 0, 10, 10]], [20, 30, 100, 50]),
            test_texture(false, vec![[0, 0, 10, 10]], [200, 30, 100, 50]),
            test_texture(true, Vec::new(), [320, 30, 100, 50]),
        ]);

        expand_dmabuf_damage(&mut scene);

        assert_eq!(
            scene.damage,
            vec![[1, 2, 3, 4], [20, 30, 100, 50], [320, 30, 100, 50]]
        );
    }

    #[test]
    fn hdr_source_peak_change_requires_full_damage_after_first_present() {
        let peak_1000 = Some(1000.0_f32.to_bits());
        let peak_4000 = Some(4000.0_f32.to_bits());

        assert!(!hdr_source_peak_changed(false, None, peak_1000));
        assert!(!hdr_source_peak_changed(true, peak_1000, peak_1000));
        assert!(hdr_source_peak_changed(true, peak_1000, peak_4000));
        assert!(hdr_source_peak_changed(true, peak_1000, None));
    }

    #[test]
    fn cursor_upload_converts_rgba_to_little_endian_argb_and_keeps_pitch_padding_clear() {
        let mut destination = vec![0xff; 16];
        copy_cursor_rgba_to_argb(&mut destination, 8, &[10, 20, 30, 40, 50, 60, 70, 80], 2, 1)
            .unwrap();
        assert_eq!(
            destination,
            [30, 20, 10, 40, 70, 60, 50, 80, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn cursor_upload_rejects_truncated_pixels() {
        assert!(copy_cursor_rgba_to_argb(&mut [0; 4], 4, &[0; 3], 1, 1).is_err());
    }
}

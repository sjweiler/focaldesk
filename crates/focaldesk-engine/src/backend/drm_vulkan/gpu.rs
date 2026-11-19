//! Per-device discovery and lifecycle for secondary Vulkan GPUs. Supports an
//! isolated SDR bring-up pattern and shared-desktop rendering with copy fallback.

pub(super) mod desktop;

use super::*;
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use std::path::Path;

#[derive(Debug)]
pub(super) struct GpuIdentity {
    pub sysfs_device: PathBuf,
    pub pci_address: Option<String>,
    pub render_path: Option<PathBuf>,
}

impl GpuIdentity {
    fn from_node(node: DrmNode) -> Result<Self> {
        let sysfs_device = std::fs::canonicalize(format!(
            "/sys/dev/char/{}:{}/device",
            node.major(),
            node.minor()
        ))
        .context("resolve stable DRM device identity")?;
        let uevent = std::fs::read_to_string(sysfs_device.join("uevent")).unwrap_or_default();
        let pci_address = pci_slot(&uevent).map(str::to_owned);
        let mut render_paths = std::fs::read_dir(sysfs_device.join("drm"))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
            .map(|entry| PathBuf::from("/dev/dri").join(entry.file_name()))
            .collect::<Vec<_>>();
        render_paths.sort();
        Ok(Self {
            sysfs_device,
            pci_address,
            render_path: render_paths.into_iter().next(),
        })
    }
}

fn pci_slot(uevent: &str) -> Option<&str> {
    uevent.lines().find_map(|line| {
        line.strip_prefix("PCI_SLOT_NAME=")
            .filter(|slot| !slot.is_empty())
    })
}

struct TestOutput {
    name: String,
    crtc: crtc::Handle,
    scanout: VulkanScanout,
    width: u32,
    height: u32,
    pending_since: Option<Instant>,
    presented: bool,
    // For the NVIDIA compatibility path, poll the fence without blocking the
    // shared event loop before giving KMS a buffer with no explicit fence.
    ready_fence: Option<KmsFence>,
    fence_started_at: Option<Instant>,
}

pub(super) struct SecondaryGpu {
    // Scanout must be released before its owning DRM device.
    outputs: Vec<TestOutput>,
    desktop: Option<desktop::SecondaryDesktop>,
    device: VulkanDevice,
    token: RegistrationToken,
    pub reconfigure_at: Option<Instant>,
    failed: bool,
    topology: u64,
    _session_device: SessionDevice,
}

struct SessionDevice {
    session: LibSeatSession,
    fd: Option<OwnedFd>,
}

impl Drop for SessionDevice {
    fn drop(&mut self) {
        if let Some(fd) = self.fd.take() {
            if let Err(error) = self.session.close(fd) {
                flog_warn!("close secondary libseat device: {error}");
            }
        }
    }
}

// Modesets and connector property writes also generate udev Changed events.
// Compare physical topology before rebuilding, or our own test commit can
// otherwise trigger an endless modeset/udev loop.
fn topology_signature(drm: &DrmDevice) -> Result<u64> {
    let mut signature = DefaultHasher::new();
    for connector in drm.resource_handles()?.connectors() {
        let info = drm.get_connector(*connector, true)?;
        if info.state() == connector::State::Connected && !info.modes().is_empty() {
            connector.hash(&mut signature);
            format!("{:?}", info.modes()).hash(&mut signature);
        }
    }
    Ok(signature.finish())
}

fn create_test_outputs(device: &mut VulkanDevice) -> Result<Vec<TestOutput>> {
    // Probe connected connectors, but ignore saved desktop/HDR choices during
    // bring-up. Use preferred SDR modes and never publish wl_output globals.
    let resources = device.drm.resource_handles()?;
    let formats = ash_formats(&device.renderer);
    let mut used_crtcs = HashSet::new();
    let mut outputs = Vec::new();
    for connector in resources.connectors() {
        let info = device.drm.get_connector(*connector, true)?;
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        let Some(crtc) = info
            .encoders()
            .iter()
            .filter_map(|encoder| device.drm.get_encoder(*encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .find(|crtc| !used_crtcs.contains(crtc))
        else {
            continue;
        };
        let mode = *info
            .modes()
            .iter()
            .find(|mode| {
                mode.mode_type()
                    .contains(smithay::reexports::drm::control::ModeTypeFlags::PREFERRED)
            })
            .unwrap_or(&info.modes()[0]);
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let surface = device.drm.create_surface(crtc, mode, &[*connector])?;
        let gbm = GbmDevice::new(device.drm_fd.clone())?;
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let scanout =
            VulkanScanout::new(surface, allocator, &SDR_SCANOUT_FORMATS, formats.clone())?;
        if !device.disable_explicit_kms_fences
            && !plane_has_input_fence(&device.drm, scanout.plane())?
        {
            return Err(anyhow!("secondary output {name} has no IN_FENCE_FD"));
        }
        // Explicitly clear inherited HDR signaling so an SDR frame is never
        // presented with the previous compositor's HDR metadata.
        let support = hdr_detection::connector_hdr_support(
            &device.drm,
            *connector,
            connector_edid(&device.drm, *connector).as_deref(),
        );
        if let Some(state) = hdr_detection::hdr_kms::build_connector_hdr_state(
            &device.drm,
            *connector,
            &support,
            false,
            None,
        )? {
            scanout.surface().use_hdr_state(state)?;
        }
        super::super::drm::reset_crtc_gamma(&device.drm, crtc)?;
        used_crtcs.insert(crtc);
        let (width, height) = mode.size();
        outputs.push(TestOutput {
            name,
            crtc,
            scanout,
            width: width.into(),
            height: height.into(),
            pending_since: None,
            presented: false,
            ready_fence: None,
            fence_started_at: None,
        });
    }
    Ok(outputs)
}

impl SecondaryGpu {
    fn service(&mut self) -> Result<()> {
        self.device.renderer.poll()?;
        if self
            .reconfigure_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if topology_signature(&self.device.drm)? == self.topology {
                self.reconfigure_at = None;
                return Ok(());
            }
            // Retire in-flight commits through their own notifier before
            // destroying swapchains. A stuck commit fails this GPU only.
            if self
                .outputs
                .iter()
                .all(|output| output.pending_since.is_none())
            {
                self.device.renderer.quiesce(Duration::from_millis(100))?;
                for index in 0..self.outputs.len() {
                    self.device.renderer.remove_stream(index as u64);
                }
                self.outputs.clear();
                self.outputs = create_test_outputs(&mut self.device)?;
                self.topology = topology_signature(&self.device.drm)?;
                self.reconfigure_at = None;
            }
        }
        for (index, output) in self.outputs.iter_mut().enumerate() {
            if let Some(started) = output.pending_since {
                if started.elapsed() >= KMS_PRESENT_TIMEOUT {
                    return Err(anyhow!("secondary KMS timeout on {}", output.name));
                }
                continue;
            }
            if output.presented {
                continue;
            }
            if output
                .fence_started_at
                .is_some_and(|started| started.elapsed() >= KMS_PRESENT_TIMEOUT)
            {
                return Err(anyhow!("secondary Vulkan fence timeout on {}", output.name));
            }
            if output.ready_fence.is_none() {
                let (dmabuf, _) = output.scanout.next_buffer()?;
                let target = target_from_dmabuf(&dmabuf)?;
                let background = test_pattern(output.width, output.height);
                let submission = self.device.renderer.render(
                    &target,
                    &background,
                    &[],
                    &[],
                    0,
                    &[],
                    0,
                    &[],
                    &[],
                    0,
                    &[[0, 0, output.width as i32, output.height as i32]],
                    index as u64,
                    None,
                    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    AshDrmTransfer::Srgb,
                    None,
                    None,
                )?;
                output.ready_fence = Some(KmsFence(submission.fence_fd));
                output.fence_started_at = Some(Instant::now());
            }
            let sync = if self.device.disable_explicit_kms_fences {
                if !output.ready_fence.as_ref().unwrap().poll(0)? {
                    // Keep the acquired buffer and fence; poll on the next
                    // dispatch without rendering or blocking again.
                    continue;
                }
                output.ready_fence.take();
                None
            } else {
                Some(SyncPoint::from(output.ready_fence.take().unwrap()))
            };
            output.scanout.queue_buffer(sync, None, ())?;
            output.fence_started_at = None;
            output.pending_since = Some(Instant::now());
        }
        Ok(())
    }

    pub fn pause(&mut self) {
        self.device.drm.pause();
    }

    pub fn resume(&mut self) {
        // Recovery is isolated from the primary renderer. A failed secondary
        // stays disabled until removal/re-addition, preserving the main session.
        let result = (|| -> Result<()> {
            self.device.drm.activate(true)?;
            self.device.renderer.quiesce(Duration::from_millis(100))?;
            self.outputs.clear();
            self.device.metrics.rebuild_started();
            self.device
                .renderer
                .recreate_quiesced(self.device.node.major(), self.device.node.minor())?;
            self.device.metrics.rebuild_succeeded();
            self.outputs = create_test_outputs(&mut self.device)?;
            self.topology = topology_signature(&self.device.drm)?;
            self.reconfigure_at = None;
            Ok(())
        })();
        self.failed = result.is_err();
        if let Err(error) = result {
            self.device.metrics.error(&error);
            self.device.metrics.failed();
            flog_warn!("secondary GPU resume failed: {error:#}");
        }
    }
}

impl Drop for SecondaryGpu {
    fn drop(&mut self) {
        if self
            .device
            .renderer
            .quiesce(Duration::from_millis(100))
            .is_err()
        {
            self.device.renderer.abandon_device();
        }
    }
}

fn test_pattern(width: u32, height: u32) -> Vec<focaldesk_render::SolidQuad> {
    let colors = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    ];
    colors
        .into_iter()
        .enumerate()
        .map(|(index, color)| {
            let left = width as u64 * index as u64 / 3;
            let right = width as u64 * (index + 1) as u64 / 3;
            focaldesk_render::SolidQuad {
                destination: [left as i32, 0, (right - left) as i32, height as i32],
                color,
                corner_radius: 0.0,
            }
        })
        .collect()
}

pub(super) fn discover_device(
    data: &mut VulkanDrmData,
    handle: &LoopHandle<'static, VulkanDrmData>,
    node: DrmNode,
    path: &Path,
) {
    let identity = match GpuIdentity::from_node(node) {
        Ok(identity) => identity,
        Err(error) => {
            flog_warn!("DRM discovery failed for {}: {error:#}", path.display());
            return;
        }
    };
    flog(format!(
        "DRM GPU discovered: card={} pci={:?} sysfs={} render={:?} primary={}",
        path.display(),
        identity.pci_address,
        identity.sysfs_device.display(),
        identity.render_path,
        node == data.primary_gpu.node
    ));
    if !(data.secondary_test_enabled || data.secondary_desktop_enabled)
        || node == data.primary_gpu.node
        || data.secondary_gpus.contains_key(&node)
    {
        return;
    }
    if !data.session_active || data.resume_pending {
        data.pending_secondary_gpus.insert(node, path.to_owned());
        return;
    }
    let result = (|| -> Result<()> {
        let fd = data
            .session
            .open(path, OFlags::RDWR | OFlags::CLOEXEC)
            .map_err(|error| anyhow!("open secondary GPU: {error}"))?;
        let session_device = SessionDevice {
            session: data.session.clone(),
            fd: Some(fd),
        };
        let fd = session_device.fd.as_ref().unwrap().try_clone()?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        if !drm.is_atomic() {
            return Err(anyhow!("secondary GPU requires atomic KMS"));
        }
        let renderer = AshDrmRenderer::new(node.major(), node.minor())?;
        let mut device = VulkanDevice {
            metrics: data.metrics.register_device(node),
            renderer,
            drm,
            drm_fd: fd,
            node,
            disable_explicit_kms_fences: disable_explicit_kms_fences(drm_card_vendor_id(path)),
        };
        let topology = topology_signature(&device.drm)?;
        let mut secondary_desktop = if data.secondary_desktop_enabled {
            Some(desktop::SecondaryDesktop::new(data, &mut device)?)
        } else {
            None
        };
        let outputs = if secondary_desktop.is_none() {
            create_test_outputs(&mut device)?
        } else {
            Vec::new()
        };
        let token = handle.insert_source(notifier, move |event, _, data| {
            if !data.session_active || data.resume_pending { return; }
            let Some(gpu) = data.secondary_gpus.get_mut(&node) else { return };
            match event {
                DrmEvent::VBlank(crtc) => {
                    if let Some(desktop) = gpu.desktop.as_mut() {
                        if let Err(error) = desktop.page_flip(crtc, &gpu.device.metrics) {
                            gpu.failed = true;
                            flog_warn!("secondary desktop page flip failed: {error:#}");
                        }
                        return;
                    }
                    let Some(output) = gpu.outputs.iter_mut().find(|output| output.crtc == crtc) else { return };
                    if output.pending_since.is_none() { return; }
                    match output.scanout.frame_submitted() {
                        Ok(_) => {
                            gpu.device.metrics.presented(&output.name);
                            output.pending_since = None;
                            output.presented = true;
                            flog(format!("Secondary Vulkan SDR test presented: gpu={node:?} output={} {}x{}", output.name, output.width, output.height));
                        }
                        Err(error) => { gpu.failed = true; flog_warn!("secondary page flip failed: {error}"); }
                    }
                }
                DrmEvent::Error(error) => { gpu.failed = true; flog_warn!("secondary DRM event failed: {error}"); }
            }
        }).map_err(|error| {
            if let Some(desktop) = secondary_desktop.as_mut() { desktop.retire(data, &mut device); }
            anyhow!("register secondary DRM notifier: {error}")
        })?;
        data.secondary_gpus.insert(
            node,
            SecondaryGpu {
                outputs,
                desktop: secondary_desktop,
                device,
                token,
                reconfigure_at: None,
                failed: false,
                topology,
                _session_device: session_device,
            },
        );
        Ok(())
    })();
    if let Err(error) = result {
        flog_warn!(
            "Secondary GPU test unavailable on {}: {error:#}; primary GPU retained",
            path.display()
        );
    }
}

pub(super) fn remove_device(
    data: &mut VulkanDrmData,
    handle: &LoopHandle<'static, VulkanDrmData>,
    node: DrmNode,
) {
    data.pending_secondary_gpus.remove(&node);
    if let Some(mut gpu) = data.secondary_gpus.remove(&node) {
        handle.remove(gpu.token);
        if let Some(desktop) = gpu.desktop.as_mut() {
            desktop.retire(data, &mut gpu.device);
        }
    }
}

pub(super) fn service_secondary_gpus(
    data: &mut VulkanDrmData,
    handle: &LoopHandle<'static, VulkanDrmData>,
) {
    if !data.session_active || data.resume_pending {
        return;
    }
    for (node, path) in std::mem::take(&mut data.pending_secondary_gpus) {
        discover_device(data, handle, node, &path);
    }
    let mut devices = std::mem::take(&mut data.secondary_gpus);
    let mut captures = Vec::new();
    for (node, gpu) in &mut devices {
        if gpu.failed {
            if let Some(desktop) = gpu.desktop.as_mut() {
                if !desktop.outputs.is_empty() {
                    desktop.retire(data, &mut gpu.device);
                }
                if gpu
                    .reconfigure_at
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    gpu.reconfigure_at = None;
                    match desktop.resume(data, &mut gpu.device) {
                        Ok(()) => {
                            gpu.failed = false;
                            gpu.topology =
                                topology_signature(&gpu.device.drm).unwrap_or(gpu.topology);
                        }
                        Err(error) => {
                            gpu.device.metrics.error(&error);
                            gpu.device.metrics.failed();
                            desktop.retire(data, &mut gpu.device);
                            flog_warn!("Secondary hotplug recovery failed: {error:#}");
                        }
                    }
                }
            }
            continue;
        }
        let result = if let Some(desktop) = gpu.desktop.as_mut() {
            (|| -> Result<()> {
                gpu.device.renderer.poll()?;
                if gpu
                    .reconfigure_at
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let topology = topology_signature(&gpu.device.drm)?;
                    if topology != gpu.topology || desktop.reconfigure_requested {
                        if desktop.outputs.iter().all(|output| !output.frame_pending) {
                            desktop.rebuild(data, &mut gpu.device)?;
                            gpu.topology = topology;
                            gpu.reconfigure_at = None;
                        }
                    } else {
                        gpu.reconfigure_at = None;
                    }
                }
                while let Some(capture) = gpu.device.renderer.take_completed_capture() {
                    captures.push(capture);
                }
                Ok(())
            })()
        } else {
            gpu.service()
        };
        if let Err(error) = result {
            gpu.device.metrics.error(&error);
            gpu.device.metrics.failed();
            gpu.failed = true;
            gpu.reconfigure_at = None;
            if let Some(desktop) = gpu.desktop.as_mut() {
                desktop.retire(data, &mut gpu.device);
            }
            flog_warn!("Secondary GPU {node:?} stopped: {error:#}; primary GPU retained");
        }
    }
    data.secondary_gpus = devices;
    for capture in captures {
        finish_vulkan_capture(data, capture);
    }
}

pub(super) fn render_secondary_desktops(data: &mut VulkanDrmData, primary_ready: bool) {
    if !data.session_active || data.resume_pending {
        return;
    }
    let mut devices = std::mem::take(&mut data.secondary_gpus);
    let mut presented = false;
    for (node, gpu) in &mut devices {
        if gpu.failed {
            continue;
        }
        let Some(desktop) = gpu.desktop.as_mut() else {
            continue;
        };
        match desktop.render(data, &mut gpu.device, primary_ready) {
            Ok(any) => presented |= any,
            Err(error) => {
                gpu.device.metrics.error(&error);
                gpu.device.metrics.failed();
                gpu.failed = true;
                gpu.reconfigure_at = None;
                desktop.retire(data, &mut gpu.device);
                flog_warn!(
                    "Secondary GPU {node:?} desktop stopped: {error:#}; primary GPU retained"
                );
            }
        }
    }
    data.secondary_gpus = devices;
    if presented {
        data.desktop.state.render.frame_no += 1;
        data.desktop
            .state
            .send_frame_callbacks(data.desktop.start.elapsed().as_millis() as u32);
    }
}

pub(super) fn complete_bridge_capture(data: &mut VulkanDrmData, capture: AshDrmCapture) {
    let Some((node, id)) = data.bridge_captures.remove(&capture.id) else {
        return;
    };
    let Some(gpu) = data.secondary_gpus.get_mut(&node) else {
        return;
    };
    let Some(desktop) = gpu.desktop.as_mut() else {
        return;
    };
    if let Err(error) = desktop.complete_bridge(capture, id) {
        gpu.failed = true;
        gpu.reconfigure_at = None;
        flog_warn!("Cross-GPU readback failed: {error:#}");
    }
}

pub(super) fn desktop_outputs(data: &VulkanDrmData) -> impl Iterator<Item = &VulkanOutput> {
    data.secondary_gpus
        .values()
        .filter(|gpu| !gpu.failed)
        .filter_map(|gpu| gpu.desktop.as_ref())
        .flat_map(|desktop| &desktop.outputs)
}

pub(super) fn resume_secondary_gpus(data: &mut VulkanDrmData) {
    let mut devices = std::mem::take(&mut data.secondary_gpus);
    for gpu in devices.values_mut() {
        if let Some(desktop) = gpu.desktop.as_mut() {
            if let Err(error) = desktop.resume(data, &mut gpu.device) {
                gpu.device.metrics.error(&error);
                gpu.device.metrics.failed();
                gpu.failed = true;
                desktop.retire(data, &mut gpu.device);
                flog_warn!("Secondary desktop resume failed: {error:#}");
            } else {
                gpu.failed = false;
            }
        } else {
            gpu.resume();
        }
    }
    data.secondary_gpus = devices;
}

pub(super) fn primary_renderer_recreated(data: &mut VulkanDrmData) {
    data.bridge_captures.clear();
    for gpu in data.secondary_gpus.values_mut() {
        if let Some(desktop) = gpu.desktop.as_mut() {
            desktop.primary_recreated();
        }
    }
}

pub(super) fn request_reconfigure(data: &mut VulkanDrmData) {
    for gpu in data.secondary_gpus.values_mut() {
        if let Some(desktop) = gpu.desktop.as_mut() {
            desktop.reconfigure_requested = true;
            gpu.reconfigure_at = Some(Instant::now());
        }
    }
}

// Keep the primary GPU's allocation feedback unchanged. A modifier supported
// by both Vulkan devices does not prove that a foreign allocation is usable on
// both. The secondary renderer handles incompatible imports with its copy path.

pub(super) fn touch_output(
    data: &VulkanDrmData,
    event: &SmithayInputEvent<LibinputInputBackend>,
) -> Option<OutputId> {
    use smithay::backend::input::Event;
    let device = match event {
        SmithayInputEvent::TouchDown { event } => event.device(),
        SmithayInputEvent::TouchMotion { event } => event.device(),
        _ => return None,
    };
    if let Some(name) = device
        .output_name()
        .map(|name| name.into_owned())
        .or_else(|| std::env::var("FOCALDESK_TOUCH_OUTPUT").ok())
    {
        return data
            .desktop
            .state
            .outputs
            .iter()
            .find_map(|(id, output)| (output.handle.name() == name).then_some(*id));
    }
    if let Some(id) = data.desktop.state.sole_control_panel_output() {
        return Some(id);
    }
    // The current bring-up setup has one touch device and one secondary
    // display. Do not guess when either side has multiple candidates.
    if data.touch_devices.len() != 1 {
        return None;
    }
    let mut outputs = data
        .secondary_gpus
        .values()
        .filter(|gpu| !gpu.failed)
        .filter_map(|gpu| gpu.desktop.as_ref())
        .flat_map(|desktop| &desktop.outputs);
    let first = outputs.next()?.output_id;
    outputs.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_identity_uses_slot_and_supports_non_pci_devices() {
        assert_eq!(
            pci_slot("DRIVER=i915\nPCI_ID=8086:4680\nPCI_SLOT_NAME=0000:00:02.0\n"),
            Some("0000:00:02.0")
        );
        assert_eq!(pci_slot("DRIVER=simple-framebuffer\n"), None);
        assert_eq!(pci_slot("PCI_SLOT_NAME=\n"), None);
    }

    #[test]
    fn test_pattern_covers_non_divisible_output_without_gaps() {
        let pattern = test_pattern(1366, 768);
        let mut edge = 0;
        for quad in pattern {
            assert_eq!(quad.destination[0], edge);
            assert_eq!(quad.destination[1], 0);
            assert_eq!(quad.destination[3], 768);
            edge += quad.destination[2];
        }
        assert_eq!(edge, 1366);
    }
}

pub(super) fn sample_metrics(data: &VulkanDrmData) {
    for gpu in data.secondary_gpus.values() {
        gpu.device.metrics.sample_gpu(gpu.failed, data.session_active);
        if let Some(desktop) = &gpu.desktop {
            for output in &desktop.outputs {
                gpu.device.metrics.display(
                    &output.name,
                    output.width,
                    output.height,
                    Some(output.refresh_millihz),
                    gpu.device.metrics.active(),
                );
            }
        } else {
            for output in &gpu.outputs {
                gpu.device.metrics.display(
                    &output.name,
                    output.width,
                    output.height,
                    None,
                    gpu.device.metrics.active(),
                );
            }
        }
    }
}

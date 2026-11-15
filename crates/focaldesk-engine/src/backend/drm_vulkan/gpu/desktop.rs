//! Desktop rendering on a secondary DRM device. Cross-device imports are tried
//! first. Incompatible client buffers use an asynchronous primary-GPU readback
//! and secondary upload; the shared event loop never waits for that copy.

use super::*;
use crate::core::desktop::{DamageSource, DesktopState};
use focaldesk_render::{AshDrmSubmission, FramePixelFormat, TextureColorTransform, TextureQuad};
use smithay::backend::allocator::gbm::GbmBuffer;
use smithay::backend::allocator::{dmabuf::AsDmabuf, Allocator};

pub(in super::super) const BRIDGE_STREAM_BASE: u64 = 1 << 62;
const DIRECT_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const BRIDGE_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

struct BridgeTarget {
    _buffer: GbmBuffer,
    target: DrmRenderTarget,
}

#[derive(Default)]
struct FrameState {
    bridge: bool,
    last_direct_attempt: Option<Instant>,
    bridge_target: Option<BridgeTarget>,
    bridge_started: Option<Instant>,
    bridge_capture: Option<u64>,
    last_bridge: Option<Instant>,
    bridge_pixels: Option<TextureQuad>,
    ready_fence: Option<KmsFence>,
    fence_started: Option<Instant>,
    retry: AllocationBackoff,
}

impl FrameState {
    fn should_probe_direct(&self, now: Instant) -> bool {
        self.bridge
            && self.bridge_pixels.is_none()
            && self.bridge_capture.is_none()
            && self.bridge_started.is_none()
            && self
                .last_direct_attempt
                .is_some_and(|last| now.saturating_duration_since(last) >= DIRECT_RETRY_INTERVAL)
    }
}

pub(super) struct SecondaryDesktop {
    pub outputs: Vec<VulkanOutput>,
    builder: VulkanSceneBuilder,
    frames: HashMap<OutputId, FrameState>,
    ids: HashMap<connector::Handle, OutputId>,
    pub reconfigure_requested: bool,
    sample_formats: HashSet<(u32, u64)>,
    feedback: smithay::wayland::dmabuf::DmabufFeedback,
}

impl SecondaryDesktop {
    pub fn new(data: &mut VulkanDrmData, device: &mut VulkanDevice) -> Result<Self> {
        let mut desktop = Self {
            feedback: secondary_feedback(data, device)?,
            outputs: Vec::new(),
            builder: VulkanSceneBuilder::default(),
            frames: HashMap::new(),
            ids: HashMap::new(),
            reconfigure_requested: false,
            sample_formats: device.renderer.sample_formats().into_iter().collect(),
        };
        if let Err(error) = desktop.initialize(data, device) {
            desktop.retire(data, device);
            return Err(error);
        }
        Ok(desktop)
    }

    fn initialize(&mut self, data: &mut VulkanDrmData, device: &mut VulkanDevice) -> Result<()> {
        let overrides = data
            .desktop
            .state
            .trial_display_outputs()
            .map(|outputs| outputs.to_vec());
        let mut configs = select_outputs_for_device(&device.drm, overrides.as_deref(), false)?;
        // Unconfigured displays extend the desktop to the right. Saved layouts,
        // including deliberately overlapping control panels, retain their origin.
        let mut next_x = data
            .desktop
            .state
            .outputs
            .values()
            .map(|output| {
                output
                    .logical_origin
                    .x
                    .saturating_add(output.logical_size.w)
            })
            .max()
            .unwrap_or(0);
        for config in &mut configs {
            config.output_id = output_id(
                &mut self.ids,
                &mut data.next_secondary_output_id,
                config.connector,
            );
            config.primary = false;
            config.hdr_requested = false;
            if !config.position_configured {
                config.origin = (next_x, 0).into();
            }
            let size = config
                .transform
                .transform_size(Size::<i32, Physical>::from((
                    config.width as i32,
                    config.height as i32,
                )));
            next_x =
                next_x.max(config.origin.x + (f64::from(size.w) / config.scale).round() as i32);
            // Connector numbers are local to a GPU; avoid duplicate wl_output
            // names if two cards both expose e.g. HDMI-A-1.
            if data
                .desktop
                .state
                .outputs
                .values()
                .any(|output| output.handle.name() == config.name)
            {
                let identity = GpuIdentity::from_node(device.node)?;
                let suffix = identity
                    .pci_address
                    .unwrap_or_else(|| format!("{}:{}", device.node.major(), device.node.minor()));
                config.name = format!("{}@{suffix}", config.name);
            }
        }
        let configured = load_display_config_with_override(overrides.as_deref());
        let inventory = merge_disconnected_display_configs(
            output_config_snapshots(&configs, &configured),
            &configured,
        );
        for config in configs {
            let id = config.output_id;
            // Initialize transactionally, one output at a time. A failed
            // secondary modeset must not leave a published phantom wl_output.
            match initialize_vulkan_outputs(
                &mut data.desktop,
                &device.renderer,
                &mut device.drm,
                &device.drm_fd,
                vec![config],
            ) {
                Ok(mut outputs) => {
                    for output in &mut outputs {
                        output.cursor = None;
                    }
                    self.outputs.extend(outputs);
                    self.frames.insert(id, FrameState::default());
                    if let Some(state) = data.desktop.state.outputs.get_mut(&id) {
                        state.hdr_supported = false;
                        state.hdr_requested = false;
                    }
                }
                Err(error) => {
                    self.retire(data, device);
                    return Err(error);
                }
            }
        }
        data.desktop.state.connected_drm_outputs.insert(
            format!("gpu-{}:{}", device.node.major(), device.node.minor()),
            super::super::super::drm::connected_output_inventory(&device.drm)?,
        );
        if overrides.is_none() {
            if let Err(error) = write_display_config(&inventory) {
                flog_warn!("Secondary Vulkan could not publish display inventory: {error:#}");
            }
        }
        data.desktop.state.mark_redraw();
        flog_warn!(
            "Secondary Vulkan desktop ready: gpu={:?} outputs={} SDR",
            device.node,
            self.outputs.len()
        );
        Ok(())
    }

    pub fn retire(&mut self, data: &mut VulkanDrmData, device: &mut VulkanDevice) {
        let snapshot = data.desktop.state.snapshot_output_topology();
        for mut output in self.outputs.drain(..) {
            let id = output.output_id;
            if output.frame_pending {
                let _ = output.scanout.frame_submitted();
            }
            data.desktop
                .display
                .handle()
                .disable_global::<DesktopState>(output.global.clone());
            data.retired_output_globals.push((
                Instant::now() + Duration::from_secs(5),
                output.global.clone(),
            ));
            if let Some(state) = data.desktop.state.outputs.get(&id) {
                data.desktop.state.space.unmap_output(&state.handle);
            }
            data.desktop.state.forget_workspace_transition(id);
            data.desktop.state.outputs.shift_remove(&id);
            data.desktop.state.desktop_outputs.shift_remove(&id);
            data.desktop.output_state.outputs.shift_remove(&id);
            data.capture_pending.remove(&id);
            data.bridge_captures.retain(|_, (_, output)| *output != id);
            data.screenshot_all_captures.remove(&id);
            device.renderer.remove_stream(id.0);
            data.primary_gpu
                .renderer
                .remove_stream(BRIDGE_STREAM_BASE | id.0);
            hdr_detection::hdr_kms::destroy_hdr_metadata_blob(
                &device.drm,
                output.hdr_metadata_blob.take(),
            );
        }
        self.frames.clear();
        self.builder.clear();
        data.desktop.state.connected_drm_outputs.remove(&format!(
            "gpu-{}:{}",
            device.node.major(),
            device.node.minor()
        ));
        if !data
            .desktop
            .state
            .outputs
            .contains_key(&data.desktop.state.focused_output)
        {
            data.desktop.state.focused_output = data.desktop.state.primary_output;
        }
        data.desktop.state.restore_output_topology(snapshot);
        data.desktop.state.mark_redraw();
    }

    pub fn rebuild(&mut self, data: &mut VulkanDrmData, device: &mut VulkanDevice) -> Result<()> {
        let snapshot = data.desktop.state.snapshot_output_topology();
        device.renderer.quiesce(Duration::from_millis(100))?;
        self.retire(data, device);
        self.initialize(data, device)?;
        data.desktop.state.restore_output_topology(snapshot);
        self.reconfigure_requested = false;
        Ok(())
    }

    pub fn resume(&mut self, data: &mut VulkanDrmData, device: &mut VulkanDevice) -> Result<()> {
        let snapshot = data.desktop.state.snapshot_output_topology();
        device.drm.activate(true)?;
        device.renderer.quiesce(Duration::from_millis(100))?;
        self.retire(data, device);
        device
            .renderer
            .recreate_quiesced(device.node.major(), device.node.minor())?;
        self.sample_formats = device.renderer.sample_formats().into_iter().collect();
        self.feedback = secondary_feedback(data, device)?;
        self.initialize(data, device)?;
        data.desktop.state.restore_output_topology(snapshot);
        Ok(())
    }

    pub fn page_flip(&mut self, crtc: crtc::Handle) -> Result<()> {
        let Some(output) = self.outputs.iter_mut().find(|output| output.crtc == crtc) else {
            return Ok(());
        };
        if !output.frame_pending {
            return Ok(());
        }
        output.scanout.frame_submitted()?;
        output.frame_pending = false;
        output.present_started_at = None;
        if std::mem::take(&mut output.log_first_present_state) {
            flog_warn!(
                "Secondary Vulkan desktop presented: output={} {}x{} SDR",
                output.name,
                output.width,
                output.height
            );
        }
        Ok(())
    }

    pub fn primary_recreated(&mut self) {
        for frame in self.frames.values_mut() {
            frame.bridge_target = None;
            frame.bridge_started = None;
            frame.bridge_capture = None;
            frame.bridge_pixels = None;
        }
    }

    pub fn complete_bridge(&mut self, capture: AshDrmCapture, id: OutputId) -> Result<()> {
        let Some(frame) = self.frames.get_mut(&id) else {
            return Ok(());
        };
        if frame.bridge_capture != Some(capture.id) {
            return Ok(());
        }
        frame.bridge_started = None;
        frame.bridge_capture = None;
        let (width, height) = (capture.width, capture.height);
        let pixels = capture.into_rgba8()?;
        frame.bridge_pixels = Some(TextureQuad {
            cache_key: BRIDGE_STREAM_BASE | id.0,
            pixels,
            width,
            height,
            stride: width * 4,
            format: FramePixelFormat::Rgba8Srgb,
            color_transform: TextureColorTransform::default(),
            dmabuf: None,
            damage: vec![[0, 0, width, height]],
            destination: [0, 0, width as i32, height as i32],
            source_uv: [0.0, 0.0, 1.0, 1.0],
            transform: FrameTransform::Normal,
            tint: [1.0; 4],
            retention: None,
        });
        Ok(())
    }

    pub fn render(
        &mut self,
        data: &mut VulkanDrmData,
        device: &mut VulkanDevice,
        primary_ready: bool,
    ) -> Result<bool> {
        let mut presented = false;
        for output in &mut self.outputs {
            let id = output.output_id;
            // Layer-shell surfaces belong to exactly one output. Prefer that
            // output's GPU without changing allocation policy for other clients.
            if let Some(state) = data.desktop.state.outputs.get(&id) {
                let map = smithay::desktop::layer_map_for_output(&state.handle);
                for layer in map.layers() {
                    layer.send_dmabuf_feedback(
                        &state.handle,
                        |_, _| Some(state.handle.clone()),
                        |_, _| &self.feedback,
                    );
                }
            }
            let frame = self.frames.get_mut(&id).unwrap();
            if output.frame_pending {
                if output
                    .present_started_at
                    .is_some_and(|started| started.elapsed() >= KMS_PRESENT_TIMEOUT)
                {
                    return Err(anyhow!("secondary KMS timeout on {}", output.name));
                }
                continue;
            }
            if frame
                .fence_started
                .is_some_and(|started| started.elapsed() >= KMS_PRESENT_TIMEOUT)
                || frame
                    .bridge_started
                    .is_some_and(|started| started.elapsed() >= KMS_PRESENT_TIMEOUT)
            {
                return Err(anyhow!("secondary rendering timeout on {}", output.name));
            }
            if let Some(fence) = frame.ready_fence.as_ref() {
                if device.disable_explicit_kms_fences && !fence.poll(0)? {
                    continue;
                }
                queue_frame(output, frame, device.disable_explicit_kms_fences)?;
                presented = true;
                continue;
            }
            if frame.bridge_started.is_some() || !frame.retry.ready(Instant::now()) {
                continue;
            }
            if frame.bridge_pixels.is_none() && !data.desktop.state.output_has_pending_damage(id) {
                continue;
            }
            // Copy mode must not be permanent: feedback can cause a client to
            // replace its NVIDIA buffers with secondary-compatible allocations.
            // Bound probes so a driver that rejects import keeps its usable copy path.
            if frame.should_probe_direct(Instant::now()) {
                frame.bridge = false;
            }
            if frame.bridge && frame.bridge_pixels.is_none() {
                if !primary_ready
                    || frame
                        .last_bridge
                        .is_some_and(|last| last.elapsed() < BRIDGE_FRAME_INTERVAL)
                {
                    continue;
                }
                match submit_bridge(data, device.node, output, frame) {
                    Ok(()) => frame.retry = AllocationBackoff::default(),
                    Err(error) if AshDrmRenderer::is_allocation_pressure(&error) => {
                        frame.retry.failed(Instant::now());
                    }
                    Err(error) => return Err(error),
                }
                continue;
            }
            let bridged = frame.bridge_pixels.is_some();
            let mut scene = if let Some(texture) = frame.bridge_pixels.take() {
                scene_from_bridge(texture, output)
            } else {
                let mut scene = self.builder.build_for_output_with_cursor_policy(
                    &mut data.desktop.state,
                    id,
                    true,
                    false,
                    true,
                );
                expand_dmabuf_damage(&mut scene);
                transform_scene(
                    &mut scene,
                    output
                        .transform
                        .invert()
                        .transform_size(Size::from((output.width as i32, output.height as i32))),
                    output.transform,
                );
                scene
            };
            // Retained scene pixels and hardware cursor state are local to each
            // GPU. A full redraw here also makes retries and fallback transitions
            // correct when a client buffer straddles two devices.
            scene.damage = vec![[0, 0, output.width as i32, output.height as i32]];
            if !bridged {
                frame.last_direct_attempt = Some(Instant::now());
            }
            if !bridged
                && scene
                    .surfaces
                    .iter()
                    .chain(&scene.egui_textures)
                    .any(|texture| {
                        texture.dmabuf.as_ref().is_some_and(|buffer| {
                            !self
                                .sample_formats
                                .contains(&(buffer.fourcc, buffer.modifier))
                        })
                    })
            {
                frame.bridge = true;
                flog_warn!("Cross-GPU copy enabled on {}: client format/modifier requires the primary renderer", output.name);
                continue;
            }
            let (dmabuf, _) = output.scanout.next_buffer()?;
            let target = target_from_dmabuf(&dmabuf)?;
            let capture = (data.desktop.state.screenshot_request() == Some(id)
                || data.desktop.state.screenshot_all_requested
                || data
                    .desktop
                    .state
                    .pending_portal_captures
                    .iter()
                    .any(|capture| capture.output_id == id)
                || (crate::core::remote::capture_active_for_output(&data.desktop.state, id)
                    && data
                        .last_cpu_capture
                        .get(&id)
                        .is_none_or(|last| last.elapsed() >= Duration::from_millis(200))))
                && !data.capture_pending.contains(&id);
            match render_sdr(
                &mut device.renderer,
                &target,
                &scene,
                id.0,
                capture.then_some(id.0),
                Some((&data.desktop.state, id)),
            ) {
                Ok(submission) => {
                    frame.retry = AllocationBackoff::default();
                    frame.ready_fence = Some(KmsFence(submission.fence_fd));
                    frame.fence_started = Some(Instant::now());
                    if !bridged {
                        if frame.last_bridge.take().is_some() {
                            flog_warn!(
                                "Secondary Vulkan direct rendering restored: output={}",
                                output.name
                            );
                        }
                        data.desktop.state.clear_output_repaint_request(id);
                    }
                    if capture {
                        data.capture_pending.insert(id);
                        data.last_cpu_capture.insert(id, Instant::now());
                    }
                    if !device.disable_explicit_kms_fences
                        || frame.ready_fence.as_ref().unwrap().poll(0)?
                    {
                        queue_frame(output, frame, device.disable_explicit_kms_fences)?;
                        presented = true;
                    }
                }
                Err(error)
                    if AshDrmRenderer::is_allocation_pressure(&error)
                        && device.renderer.is_usable() =>
                {
                    frame.retry.failed(Instant::now());
                    data.desktop
                        .state
                        .mark_output_full_damage(id, DamageSource::Unknown);
                }
                Err(error)
                    if device.renderer.is_usable()
                        && !bridged
                        && scene
                            .surfaces
                            .iter()
                            .chain(&scene.egui_textures)
                            .any(|texture| texture.dmabuf.is_some()) =>
                {
                    frame.bridge = true;
                    // Force the independent primary scene builder to include
                    // the same clients and uploads; never read tiled DMA-BUF
                    // memory directly on the CPU.
                    data.desktop
                        .state
                        .mark_output_full_damage(id, DamageSource::Unknown);
                    flog_warn!(
                        "Cross-GPU import failed on {}; using asynchronous copy: {error:#}",
                        output.name
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(presented)
    }
}

// Only advertise pairs accepted by both import validation on the primary and
// sampling on the secondary. The main device tells clients where to allocate;
// formats alone cannot make a vendor-specific NVIDIA allocation Intel-local.
fn secondary_feedback(
    data: &VulkanDrmData,
    device: &VulkanDevice,
) -> Result<smithay::wayland::dmabuf::DmabufFeedback> {
    let formats = device
        .renderer
        .sample_formats()
        .into_iter()
        .collect::<HashSet<_>>();
    let compatible = data
        .desktop
        .state
        .wgpu_dmabuf_formats
        .iter()
        .copied()
        .filter(|format| formats.contains(&(format.code as u32, u64::from(format.modifier))))
        .collect::<Vec<_>>();
    // With no shared formats retain primary allocation and the copy fallback.
    let (node, formats) = if compatible.is_empty() {
        (
            data.primary_gpu.node,
            data.desktop.state.wgpu_dmabuf_formats.clone(),
        )
    } else {
        (device.node, compatible)
    };
    flog_warn!(
        "Secondary Vulkan allocation feedback: output_gpu={:?} allocation_gpu={:?} compatible_formats={}",
        device.node, node, formats.len()
    );
    DmabufFeedbackBuilder::new(node.dev_id(), formats)
        .build()
        .context("build secondary output DMA-BUF feedback")
}

fn queue_frame(output: &mut VulkanOutput, frame: &mut FrameState, implicit: bool) -> Result<()> {
    let fence = frame.ready_fence.take().unwrap();
    let sync = (!implicit).then(|| SyncPoint::from(fence));
    output.scanout.queue_buffer(sync, None, ())?;
    frame.fence_started = None;
    output.frame_pending = true;
    output.present_started_at = Some(Instant::now());
    Ok(())
}

fn scene_from_bridge(texture: TextureQuad, output: &VulkanOutput) -> VulkanCompositorScene {
    VulkanCompositorScene {
        background: Vec::new(),
        surfaces: vec![texture],
        overlay: Vec::new(),
        overlay_after_surface: 1,
        foreground: Vec::new(),
        foreground_after_surface: 1,
        egui_textures: Vec::new(),
        egui_meshes: Vec::new(),
        egui_before_surface: 1,
        damage: vec![[0, 0, output.width as i32, output.height as i32]],
        client_surface_count: 0,
        cursor_present: true,
    }
}

fn submit_bridge(
    data: &mut VulkanDrmData,
    node: DrmNode,
    output: &VulkanOutput,
    frame: &mut FrameState,
) -> Result<()> {
    if frame.bridge_target.is_none() {
        let gbm = GbmDevice::new(data.primary_gpu.drm_fd.clone())?;
        let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
        let formats = data.primary_gpu.renderer.render_formats();
        let mut buffer = None;
        for code in SDR_SCANOUT_FORMATS {
            let modifiers = formats
                .iter()
                .filter_map(|(format, modifier)| {
                    (*format == code as u32).then_some(Modifier::from(*modifier))
                })
                .collect::<Vec<_>>();
            if modifiers.is_empty() {
                continue;
            }
            if let Ok(allocated) =
                allocator.create_buffer(output.width, output.height, code, &modifiers)
            {
                buffer = Some(allocated);
                break;
            }
        }
        let buffer = buffer.context("allocate primary-GPU cross-device copy target")?;
        let dmabuf = buffer.export()?;
        frame.bridge_target = Some(BridgeTarget {
            target: target_from_dmabuf(&dmabuf)?,
            _buffer: buffer,
        });
    }
    let mut scene = data.scene_builder.build_for_output_with_cursor_policy(
        &mut data.desktop.state,
        output.output_id,
        true,
        false,
        true,
    );
    expand_dmabuf_damage(&mut scene);
    transform_scene(
        &mut scene,
        output
            .transform
            .invert()
            .transform_size(Size::from((output.width as i32, output.height as i32))),
        output.transform,
    );
    scene.damage = vec![[0, 0, output.width as i32, output.height as i32]];
    let stream = BRIDGE_STREAM_BASE | output.output_id.0;
    let capture_id = BRIDGE_STREAM_BASE | data.next_bridge_capture_id;
    data.next_bridge_capture_id += 1;
    let _submission = render_sdr(
        &mut data.primary_gpu.renderer,
        &frame.bridge_target.as_ref().unwrap().target,
        &scene,
        stream,
        Some(capture_id),
        None,
    )?;
    data.bridge_captures
        .insert(capture_id, (node, output.output_id));
    frame.bridge_capture = Some(capture_id);
    frame.bridge_started = Some(Instant::now());
    frame.last_bridge = Some(Instant::now());
    // Subsequent client commits retain their own damage while this copy is in
    // flight. Uploading the completed copy must not clear that newer damage.
    data.desktop
        .state
        .clear_output_repaint_request(output.output_id);
    Ok(())
}

fn render_sdr(
    renderer: &mut AshDrmRenderer,
    target: &DrmRenderTarget,
    scene: &VulkanCompositorScene,
    stream: u64,
    capture: Option<u64>,
    output: Option<(&DesktopState, OutputId)>,
) -> Result<AshDrmSubmission> {
    let state = output.and_then(|(state, id)| state.outputs.get(&id));
    let matrix = state
        .map(|state| {
            crate::core::color::scene_to_output_matrix(
                state.color_description,
                crate::core::color::RenderingIntent::Relative,
            )
        })
        .unwrap_or(IDENTITY);
    let lut = state
        .and_then(|state| state.output_icc_lut.as_ref())
        .filter(|_| crate::core::icc_lut::icc_lut_shader_enabled())
        .map(|lut| AshDrmOutputLut {
            grid_size: lut.grid_size,
            rgb: &lut.rgb,
        });
    let transfer = match state.map(|state| state.color_description.transfer) {
        Some(crate::core::color::TransferFunction::Gamma22) => AshDrmTransfer::Gamma22,
        _ => AshDrmTransfer::Srgb,
    };
    renderer.render(
        target,
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
        stream,
        capture,
        matrix,
        transfer,
        lut,
        None,
    )
}

// Connector handles are only unique within a DRM device. Each device retains
// its own connector mapping, while the allocator is shared by the desktop.
fn output_id(
    ids: &mut HashMap<connector::Handle, OutputId>,
    next: &mut u64,
    connector: connector::Handle,
) -> OutputId {
    *ids.entry(connector).or_insert_with(|| {
        let id = OutputId(*next);
        *next += 1;
        id
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_desktop(id: OutputId, capture: u64) -> SecondaryDesktop {
        let mut frames = HashMap::new();
        frames.insert(
            id,
            FrameState {
                bridge: true,
                bridge_capture: Some(capture),
                bridge_started: Some(Instant::now()),
                ..FrameState::default()
            },
        );
        SecondaryDesktop {
            outputs: Vec::new(),
            builder: VulkanSceneBuilder::default(),
            frames,
            ids: HashMap::new(),
            reconfigure_requested: false,
            sample_formats: HashSet::new(),
            feedback: DmabufFeedbackBuilder::new(0, []).build().unwrap(),
        }
    }

    fn capture(id: u64) -> AshDrmCapture {
        AshDrmCapture {
            id,
            width: 2,
            height: 1,
            fourcc: Fourcc::Abgr8888 as u32,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255],
            output_transfer: AshDrmTransfer::Srgb,
            reference_white_nits: 100.0,
        }
    }

    #[test]
    fn direct_retry_is_bounded_and_preserves_pending_copies() {
        let now = Instant::now();
        let mut frame = FrameState {
            bridge: true,
            last_direct_attempt: Some(now),
            ..FrameState::default()
        };
        assert!(!frame.should_probe_direct(now));
        let retry = now + DIRECT_RETRY_INTERVAL;
        assert!(frame.should_probe_direct(retry));
        frame.bridge_capture = Some(1);
        assert!(!frame.should_probe_direct(retry));
        frame.bridge_capture = None;
        frame.bridge_started = Some(now);
        assert!(!frame.should_probe_direct(retry));
        frame.bridge_started = None;
        frame.bridge = false;
        assert!(!frame.should_probe_direct(retry));
    }

    #[test]
    fn identical_connector_handles_on_different_gpus_get_distinct_stable_output_ids() {
        let connector = connector::Handle::from(std::num::NonZeroU32::new(42).unwrap());
        let mut next = 1 << 32;
        let (mut intel, mut amd) = (HashMap::new(), HashMap::new());
        let first = output_id(&mut intel, &mut next, connector);
        let second = output_id(&mut amd, &mut next, connector);
        assert_ne!(first, second);
        assert!(first.0 > u32::MAX as u64 && second.0 < BRIDGE_STREAM_BASE);
        assert_eq!(output_id(&mut intel, &mut next, connector), first);
        assert_eq!(output_id(&mut amd, &mut next, connector), second);
        assert_eq!(next, (1 << 32) + 2);
    }

    #[test]
    fn late_copy_from_retired_output_does_not_replace_new_in_flight_frame() {
        let id = OutputId(1 << 32);
        let mut desktop = pending_desktop(id, BRIDGE_STREAM_BASE | 2);
        desktop
            .complete_bridge(capture(BRIDGE_STREAM_BASE | 1), id)
            .unwrap();
        let frame = &desktop.frames[&id];
        assert_eq!(frame.bridge_capture, Some(BRIDGE_STREAM_BASE | 2));
        assert!(frame.bridge_started.is_some());
        assert!(frame.bridge_pixels.is_none());
        desktop
            .complete_bridge(capture(BRIDGE_STREAM_BASE | 2), id)
            .unwrap();
        let frame = &desktop.frames[&id];
        assert!(frame.bridge_capture.is_none() && frame.bridge_started.is_none());
        let texture = frame.bridge_pixels.as_ref().unwrap();
        assert_eq!((texture.width, texture.height, texture.stride), (2, 1, 8));
        assert!(texture.dmabuf.is_none());
        assert_eq!(texture.pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn primary_reset_cancels_copies_without_disabling_secondary_copy_mode() {
        let id = OutputId(1 << 32);
        let mut desktop = pending_desktop(id, BRIDGE_STREAM_BASE | 1);
        desktop.primary_recreated();
        desktop
            .complete_bridge(capture(BRIDGE_STREAM_BASE | 1), id)
            .unwrap();
        let frame = &desktop.frames[&id];
        assert!(frame.bridge);
        assert!(frame.bridge_started.is_none() && frame.bridge_capture.is_none());
        assert!(frame.bridge_pixels.is_none());
    }
}

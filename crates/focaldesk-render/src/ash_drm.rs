//! Raw Vulkan renderer for compositor-owned DRM/GBM scanout buffers.
//!
//! This module deliberately has no Vulkan WSI surface or swapchain. KMS owns
//! presentation; Vulkan only imports DMA-BUF images, records rendering, and
//! exports a sync-file for the atomic commit's `IN_FENCE_FD`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::fmt;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ash::{ext, khr, vk, Entry};

use crate::{
    DrmRenderTarget, FramePixelFormat, FrameRetention, FrameTransform, MeshVertex, RendererInfo,
    SolidQuad, TextureQuad, TexturedMesh,
};

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
const XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
const ABGR8888: u32 = u32::from_le_bytes(*b"AB24");
const GPU_SUBMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const DAMAGE_HISTORY_LIMIT: usize = 64;
type DamageRect = [i32; 4];
type DamageHistory = VecDeque<(u64, Vec<DamageRect>)>;

const SOLID_SHADER: &str = r#"
struct Push { rect: vec4<f32>, color: vec4<f32> }
var<immediate> pc: Push;

struct Out { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> }

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0));
    let p = corners[i];
    var out: Out;
    out.position = vec4(pc.rect.xy + p * pc.rect.zw, 0.0, 1.0);
    out.color = pc.color;
    return out;
}

@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> { return in.color; }
"#;

const TEXTURE_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Push {
    rect: vec4<f32>,
    uv0: vec4<f32>,
    uv1: vec4<f32>,
    tint: vec4<f32>,
}
var<immediate> pc: Push;

struct Out { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec4<f32> }

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0));
    let uvs = array<vec2<f32>, 6>(pc.uv0.xy, pc.uv1.zw, pc.uv1.xy, pc.uv0.xy, pc.uv1.xy, pc.uv0.zw);
    let p = corners[i];
    var out: Out;
    out.position = vec4(pc.rect.xy + p * pc.rect.zw, 0.0, 1.0);
    out.uv = uvs[i];
    out.tint = pc.tint;
    return out;
}

@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, in.uv) * in.tint;
}
"#;

const MESH_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Push { screen: vec4<f32> }
var<immediate> pc: Push;

struct Out { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) color: vec4<f32> }

@vertex fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> Out {
    var out: Out;
    out.position = vec4(position * pc.screen.xy + pc.screen.zw, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, in.uv) * in.color;
}
"#;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct TargetKey {
    device: u64,
    inode: u64,
    modifier: u64,
    width: u32,
    height: u32,
    fourcc: u32,
}

#[derive(Clone, Copy)]
struct ImageResource {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
}

#[derive(Clone, Copy)]
struct BufferResource {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

struct PreparedMesh {
    vertex: vk::Buffer,
    index: vk::Buffer,
    index_count: u32,
}

struct TextureResource {
    image: ImageResource,
    descriptor_set: vk::DescriptorSet,
    foreign: bool,
}

struct CachedTexture {
    resource: TextureResource,
    width: u32,
    height: u32,
    format: FramePixelFormat,
    modifier: Option<u64>,
    last_used_frame: u64,
}

#[derive(Clone, Copy)]
struct DrawTexture {
    image: vk::Image,
    descriptor_set: vk::DescriptorSet,
    foreign: bool,
}

struct ImportedTarget {
    image: ImageResource,
    initialized: bool,
    last_frame: u64,
}

struct PendingFrame {
    submitted_at: Instant,
    fence: vk::Fence,
    command: vk::CommandBuffer,
    semaphore: vk::Semaphore,
    framebuffer: vk::Framebuffer,
    retired_textures: Vec<TextureResource>,
    staging: Vec<BufferResource>,
    capture: Option<PendingCapture>,
    _retentions: Vec<FrameRetention>,
}

struct PendingCapture {
    id: u64,
    buffer: BufferResource,
    byte_len: usize,
    width: u32,
    height: u32,
    fourcc: u32,
}

struct Pipelines {
    render_pass: vk::RenderPass,
    solid: vk::Pipeline,
    texture: vk::Pipeline,
    mesh: vk::Pipeline,
}

/// Completed raw-Vulkan submission and the sync-file KMS must wait on.
#[derive(Debug)]
pub struct AshDrmSubmission {
    pub fence_fd: OwnedFd,
}

/// CPU-visible encoded-SDR pixels copied from a completed Vulkan output frame.
#[derive(Debug)]
pub struct AshDrmCapture {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub pixels: Vec<u8>,
}

impl AshDrmCapture {
    /// Convert the DRM byte layout into ordinary encoded-sRGB RGBA8 pixels.
    pub fn into_rgba8(mut self) -> Result<Vec<u8>> {
        match self.fourcc {
            XRGB8888 | ARGB8888 => {
                for pixel in self.pixels.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    pixel[3] = 255;
                }
            }
            XBGR8888 | ABGR8888 => {
                for pixel in self.pixels.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
            }
            fourcc => bail!("unsupported Vulkan capture fourcc 0x{fourcc:08x}"),
        }
        Ok(self.pixels)
    }
}

/// Raw ash renderer. It never acquires a connector or creates a Vulkan surface.
pub struct AshDrmRenderer {
    _entry: Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    external_memory_fd: khr::external_memory_fd::Device,
    external_semaphore_fd: khr::external_semaphore_fd::Device,
    command_pool: vk::CommandPool,
    descriptor_pool: vk::DescriptorPool,
    descriptor_layout: vk::DescriptorSetLayout,
    solid_layout: vk::PipelineLayout,
    texture_layout: vk::PipelineLayout,
    sampler: vk::Sampler,
    pipelines: HashMap<vk::Format, Pipelines>,
    targets: HashMap<TargetKey, ImportedTarget>,
    texture_cache: HashMap<u64, CachedTexture>,
    frame_no: u64,
    pending: Vec<PendingFrame>,
    completed_captures: VecDeque<AshDrmCapture>,
    damage_history: HashMap<u64, DamageHistory>,
    stream_frames: HashMap<u64, u64>,
    /// Skip Vulkan destruction when the driver stopped making progress. A
    /// blocking `device_wait_idle` during unwinding would otherwise prevent
    /// the display manager from restarting the compositor.
    abandon_on_drop: bool,
    info: RendererInfo,
}

impl fmt::Debug for AshDrmRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AshDrmRenderer")
            .field("queue_family", &self.queue_family)
            .field("targets", &self.targets.len())
            .field("textures", &self.texture_cache.len())
            .field("pending", &self.pending.len())
            .field("info", &self.info)
            .finish()
    }
}

impl AshDrmRenderer {
    /// Create a renderer matching the DRM device major/minor numbers.
    pub fn new(drm_major: u32, drm_minor: u32) -> Result<Self> {
        let entry = unsafe { Entry::load() }.context("load Vulkan loader")?;
        let app_name = CString::new("focaldesk-ash-drm")?;
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .api_version(vk::API_VERSION_1_2);
        let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        let instance = unsafe { entry.create_instance(&create_info, None) }
            .context("create Vulkan instance")?;

        let mut selected = None;
        for physical in unsafe { instance.enumerate_physical_devices() }? {
            let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
            let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
            unsafe { instance.get_physical_device_properties2(physical, &mut props) };
            let properties = props.properties;
            let primary_matches = drm.has_primary != 0
                && drm.primary_major == i64::from(drm_major)
                && drm.primary_minor == i64::from(drm_minor);
            let render_matches = drm.has_render != 0
                && drm.render_major == i64::from(drm_major)
                && drm.render_minor == i64::from(drm_minor);
            if primary_matches || render_matches {
                selected = Some((physical, properties));
                break;
            }
        }
        let (physical_device, properties) = selected.ok_or_else(|| {
            anyhow!("no Vulkan physical device matches DRM node {drm_major}:{drm_minor}")
        })?;

        let extensions =
            unsafe { instance.enumerate_device_extension_properties(physical_device) }?;
        let has_extension = |name: &CStr| {
            extensions.iter().any(|extension| unsafe {
                CStr::from_ptr(extension.extension_name.as_ptr()) == name
            })
        };
        let required = [
            ext::image_drm_format_modifier::NAME,
            ext::external_memory_dma_buf::NAME,
            khr::external_memory_fd::NAME,
            khr::external_semaphore_fd::NAME,
        ];
        for extension in required {
            ensure!(
                has_extension(extension),
                "Vulkan device lacks {}",
                extension.to_string_lossy()
            );
        }

        let queue_family =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) }
                .iter()
                .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .ok_or_else(|| anyhow!("Vulkan device has no graphics queue"))? as u32;
        let priorities = [1.0];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let extension_names = required.map(CStr::as_ptr);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&extension_names);
        let device = unsafe { instance.create_device(physical_device, &device_info, None) }
            .context("create Vulkan DRM render device")?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let external_memory_fd = khr::external_memory_fd::Device::new(&instance, &device);
        let external_semaphore_fd = khr::external_semaphore_fd::Device::new(&instance, &device);
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }?;
        let descriptor_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(0)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(1)
                        .descriptor_type(vk::DescriptorType::SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                ]),
                None,
            )
        }?;
        let descriptor_pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(4096)
                    .pool_sizes(&[
                        vk::DescriptorPoolSize {
                            ty: vk::DescriptorType::SAMPLED_IMAGE,
                            descriptor_count: 4096,
                        },
                        vk::DescriptorPoolSize {
                            ty: vk::DescriptorType::SAMPLER,
                            descriptor_count: 4096,
                        },
                    ]),
                None,
            )
        }?;
        let solid_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(32)];
        let texture_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(64)];
        let solid_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&solid_range),
                None,
            )
        }?;
        let layouts = [descriptor_layout];
        let texture_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&layouts)
                    .push_constant_ranges(&texture_range),
                None,
            )
        }?;
        let sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
        }?;

        let adapter_name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            queue_family,
            memory_properties,
            external_memory_fd,
            external_semaphore_fd,
            command_pool,
            descriptor_pool,
            descriptor_layout,
            solid_layout,
            texture_layout,
            sampler,
            pipelines: HashMap::new(),
            targets: HashMap::new(),
            texture_cache: HashMap::new(),
            frame_no: 0,
            pending: Vec::new(),
            completed_captures: VecDeque::new(),
            damage_history: HashMap::new(),
            stream_frames: HashMap::new(),
            abandon_on_drop: false,
            info: RendererInfo {
                api: crate::GraphicsApi::Vulkan,
                adapter_name,
                driver: format!("0x{:x}", properties.driver_version),
                driver_info: "raw ash DRM DMA-BUF renderer".into(),
                surface_format: "GBM DMA-BUF".into(),
            },
        })
    }

    pub fn info(&self) -> &RendererInfo {
        &self.info
    }

    /// Formats the renderer can use as GBM scanout render targets.
    pub fn render_formats(&self) -> Vec<(u32, u64)> {
        self.formats_for_usage(
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )
    }

    /// Formats accepted when importing client DMA-BUFs for sampling.
    pub fn sample_formats(&self) -> Vec<(u32, u64)> {
        self.formats_for_usage(vk::ImageUsageFlags::SAMPLED)
    }

    fn formats_for_usage(&self, usage: vk::ImageUsageFlags) -> Vec<(u32, u64)> {
        let mut formats = Vec::new();
        // The raw DRM path currently composites the established encoded-SDR
        // scene directly into an UNORM KMS buffer. Sampling through an SRGB
        // view would decode clients/wallpaper/UI to linear without a matching
        // output encode, making the entire desktop severely dark.
        for (fourcc, render_format, sample_format) in [
            (
                XRGB8888,
                vk::Format::B8G8R8A8_UNORM,
                vk::Format::B8G8R8A8_UNORM,
            ),
            (
                ARGB8888,
                vk::Format::B8G8R8A8_UNORM,
                vk::Format::B8G8R8A8_UNORM,
            ),
            (
                XBGR8888,
                vk::Format::R8G8B8A8_UNORM,
                vk::Format::R8G8B8A8_UNORM,
            ),
            (
                ABGR8888,
                vk::Format::R8G8B8A8_UNORM,
                vk::Format::R8G8B8A8_UNORM,
            ),
        ] {
            let format = if usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
                render_format
            } else {
                sample_format
            };
            let mut count = vk::DrmFormatModifierPropertiesListEXT::default();
            let mut props = vk::FormatProperties2::default().push_next(&mut count);
            unsafe {
                self.instance.get_physical_device_format_properties2(
                    self.physical_device,
                    format,
                    &mut props,
                )
            };
            let mut values = Vec::with_capacity(count.drm_format_modifier_count as usize);
            let mut list = vk::DrmFormatModifierPropertiesListEXT {
                drm_format_modifier_count: count.drm_format_modifier_count,
                p_drm_format_modifier_properties: values.as_mut_ptr(),
                ..Default::default()
            };
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            unsafe {
                self.instance.get_physical_device_format_properties2(
                    self.physical_device,
                    format,
                    &mut props,
                );
                values.set_len(list.drm_format_modifier_count as usize);
            }
            formats.extend(values.into_iter().filter_map(|modifier| {
                let mut required_feature = vk::FormatFeatureFlags::empty();
                if usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
                    required_feature |= vk::FormatFeatureFlags::COLOR_ATTACHMENT;
                }
                if usage.contains(vk::ImageUsageFlags::SAMPLED) {
                    required_feature |= vk::FormatFeatureFlags::SAMPLED_IMAGE;
                }
                if usage.contains(vk::ImageUsageFlags::TRANSFER_SRC) {
                    required_feature |= vk::FormatFeatureFlags::TRANSFER_SRC;
                }
                if !modifier
                    .drm_format_modifier_tiling_features
                    .contains(required_feature)
                    || modifier.drm_format_modifier_plane_count != 1
                {
                    return None;
                }
                let mut drm_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
                    .drm_format_modifier(modifier.drm_format_modifier)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE);
                let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
                let image_info = vk::PhysicalDeviceImageFormatInfo2::default()
                    .format(format)
                    .ty(vk::ImageType::TYPE_2D)
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(usage)
                    .push_next(&mut drm_info)
                    .push_next(&mut external_info);
                let mut external_properties = vk::ExternalImageFormatProperties::default();
                let mut properties =
                    vk::ImageFormatProperties2::default().push_next(&mut external_properties);
                let importable = unsafe {
                    self.instance.get_physical_device_image_format_properties2(
                        self.physical_device,
                        &image_info,
                        &mut properties,
                    )
                }
                .is_ok()
                    && external_properties
                        .external_memory_properties
                        .external_memory_features
                        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE);
                importable.then_some((fourcc, modifier.drm_format_modifier))
            }));
        }
        formats.sort_unstable();
        formats.dedup();
        formats
    }

    pub fn poll(&mut self) -> Result<()> {
        let mut index = 0;
        while index < self.pending.len() {
            let reached = match unsafe { self.device.get_fence_status(self.pending[index].fence) } {
                Ok(reached) => reached,
                Err(error) => {
                    self.abandon_on_drop = true;
                    return Err(anyhow!("Vulkan submission status failed: {error}"));
                }
            };
            if reached {
                let mut frame = self.pending.swap_remove(index);
                if let Some(capture) = frame.capture.take() {
                    let completed = self.read_completed_capture(&capture);
                    self.destroy_buffer(capture.buffer);
                    match completed {
                        Ok(completed) => self.completed_captures.push_back(completed),
                        Err(error) => {
                            self.destroy_pending(frame);
                            return Err(error.context("read completed Vulkan capture"));
                        }
                    }
                }
                self.destroy_pending(frame);
            } else if self.pending[index].submitted_at.elapsed() >= GPU_SUBMISSION_TIMEOUT {
                self.abandon_on_drop = true;
                bail!(
                    "Vulkan GPU submission exceeded {} ms; abandoning the device for a clean compositor restart",
                    GPU_SUBMISSION_TIMEOUT.as_millis()
                );
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    pub fn take_completed_capture(&mut self) -> Option<AshDrmCapture> {
        self.completed_captures.pop_front()
    }

    /// Prevent potentially blocking driver teardown after a timeout or device
    /// loss. The backend replaces this renderer with a fresh Vulkan device.
    pub fn abandon_device(&mut self) {
        self.abandon_on_drop = true;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        target: &DrmRenderTarget,
        background: &[SolidQuad],
        surfaces: &[TextureQuad],
        overlay: &[SolidQuad],
        overlay_after_surface: usize,
        foreground: &[SolidQuad],
        foreground_after_surface: usize,
        mesh_textures: &[TextureQuad],
        meshes: &[TexturedMesh],
        mesh_before_surface: usize,
        damage: &[[i32; 4]],
        stream_id: u64,
        capture_id: Option<u64>,
    ) -> Result<AshDrmSubmission> {
        self.poll()?;
        ensure!(
            target.width > 0 && target.height > 0,
            "zero-sized DRM render target"
        );
        let format = vk_format(target.fourcc)?;
        self.ensure_pipelines(format)?;
        let target_key = target_key(target)?;
        if !self.targets.contains_key(&target_key) {
            let image = self.import_dmabuf_image(
                target,
                format,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            self.targets.insert(
                target_key,
                ImportedTarget {
                    image,
                    initialized: false,
                    last_frame: 0,
                },
            );
        }
        let target_image = self.targets[&target_key].image.image;
        let target_view = self.targets[&target_key].image.view;
        let initialized = self.targets[&target_key].initialized;
        let last_target_frame = self.targets[&target_key].last_frame;

        self.frame_no = self.frame_no.wrapping_add(1);
        let stream_frame = self
            .stream_frames
            .entry(stream_id)
            .and_modify(|frame| *frame = frame.wrapping_add(1))
            .or_insert(1);
        let stream_frame = *stream_frame;
        let history = self.damage_history.entry(stream_id).or_default();
        let render_areas = effective_damage_regions(
            initialized,
            last_target_frame,
            history,
            damage,
            target.width,
            target.height,
        );
        history.push_back((stream_frame, damage.to_vec()));
        while history.len() > DAMAGE_HISTORY_LIMIT {
            history.pop_front();
        }

        let command = unsafe {
            self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?
        }[0];
        unsafe {
            self.device
                .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())
        }?;

        let mut textures = Vec::with_capacity(surfaces.len());
        let mut retired_textures = Vec::new();
        let mut staging = Vec::new();
        for surface in surfaces {
            let modifier = surface.dmabuf.as_ref().map(|dmabuf| dmabuf.modifier);
            let compatible = self
                .texture_cache
                .get(&surface.cache_key)
                .is_some_and(|cached| {
                    cached.width == surface.width
                        && cached.height == surface.height
                        && cached.format == surface.format
                        && cached.modifier == modifier
                });
            if !compatible || !surface.damage.is_empty() {
                if let Some(resource) = self.prepare_texture(command, surface, &mut staging)? {
                    let cached = CachedTexture {
                        resource,
                        width: surface.width,
                        height: surface.height,
                        format: surface.format,
                        modifier,
                        last_used_frame: self.frame_no,
                    };
                    if let Some(old) = self.texture_cache.insert(surface.cache_key, cached) {
                        retired_textures.push(old.resource);
                    }
                }
            }
            let draw = self
                .texture_cache
                .get_mut(&surface.cache_key)
                .map(|cached| {
                    cached.last_used_frame = self.frame_no;
                    DrawTexture {
                        image: cached.resource.image.image,
                        descriptor_set: cached.resource.descriptor_set,
                        foreign: cached.resource.foreign,
                    }
                });
            textures.push(draw);
        }
        for texture in mesh_textures {
            let compatible = self
                .texture_cache
                .get(&texture.cache_key)
                .is_some_and(|cached| {
                    cached.width == texture.width
                        && cached.height == texture.height
                        && cached.format == texture.format
                        && cached.modifier.is_none()
                });
            if !compatible || !texture.damage.is_empty() {
                if let Some(resource) = self.prepare_texture(command, texture, &mut staging)? {
                    let cached = CachedTexture {
                        resource,
                        width: texture.width,
                        height: texture.height,
                        format: texture.format,
                        modifier: None,
                        last_used_frame: self.frame_no,
                    };
                    if let Some(old) = self.texture_cache.insert(texture.cache_key, cached) {
                        retired_textures.push(old.resource);
                    }
                }
            }
        }
        let mesh_draws = meshes
            .iter()
            .map(|mesh| {
                self.texture_cache.get_mut(&mesh.texture_key).map(|cached| {
                    cached.last_used_frame = self.frame_no;
                    DrawTexture {
                        image: cached.resource.image.image,
                        descriptor_set: cached.resource.descriptor_set,
                        foreign: cached.resource.foreign,
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut prepared_meshes = Vec::with_capacity(meshes.len());
        for mesh in meshes {
            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                prepared_meshes.push(None);
                continue;
            }
            let vertex = self.create_host_buffer(
                as_bytes(&mesh.vertices),
                vk::BufferUsageFlags::VERTEX_BUFFER,
            )?;
            let index = self
                .create_host_buffer(as_bytes(&mesh.indices), vk::BufferUsageFlags::INDEX_BUFFER)?;
            prepared_meshes.push(Some(PreparedMesh {
                vertex: vertex.buffer,
                index: index.buffer,
                index_count: mesh.indices.len() as u32,
            }));
            staging.push(vertex);
            staging.push(index);
        }
        let active_keys = surfaces
            .iter()
            .map(|surface| surface.cache_key)
            .chain(meshes.iter().map(|mesh| mesh.texture_key))
            .collect::<HashSet<_>>();
        self.texture_cache.retain(|key, cached| {
            let keep = active_keys.contains(key)
                || self.frame_no.wrapping_sub(cached.last_used_frame) <= 120;
            if !keep {
                retired_textures.push(std::mem::replace(
                    &mut cached.resource,
                    TextureResource::null(),
                ));
            }
            keep
        });

        let foreign_acquires = textures
            .iter()
            .flatten()
            .filter(|texture| texture.foreign)
            .map(|texture| texture.image)
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|image| {
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .dst_queue_family_index(self.queue_family)
                    .image(image)
                    .subresource_range(color_range())
            })
            .collect::<Vec<_>>();
        if !foreign_acquires.is_empty() {
            unsafe {
                self.device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &foreign_acquires,
                );
            }
        }

        let acquire = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_READ)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .old_layout(if initialized {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(if initialized {
                vk::QUEUE_FAMILY_FOREIGN_EXT
            } else {
                vk::QUEUE_FAMILY_IGNORED
            })
            .dst_queue_family_index(if initialized {
                self.queue_family
            } else {
                vk::QUEUE_FAMILY_IGNORED
            })
            .image(target_image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[acquire],
            );
        }

        let framebuffer = unsafe {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.pipelines[&format].render_pass)
                    .attachments(&[target_view])
                    .width(target.width)
                    .height(target.height)
                    .layers(1),
                None,
            )
        }?;
        for render_area in render_areas {
            self.record_scene_pass(
                command,
                framebuffer,
                format,
                render_area,
                target.width,
                target.height,
                background,
                surfaces,
                &textures,
                overlay,
                overlay_after_surface,
                foreground,
                foreground_after_surface,
                meshes,
                &mesh_draws,
                &prepared_meshes,
                mesh_before_surface,
            );
        }

        let foreign_releases = textures
            .iter()
            .flatten()
            .filter(|texture| texture.foreign)
            .map(|texture| texture.image)
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|image| {
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_READ)
                    .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(self.queue_family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .image(image)
                    .subresource_range(color_range())
            })
            .collect::<Vec<_>>();
        if !foreign_releases.is_empty() {
            unsafe {
                self.device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &foreign_releases,
                );
            }
        }

        let capture = if let Some(id) = capture_id {
            let byte_len = usize::try_from(target.width)
                .ok()
                .and_then(|width| {
                    usize::try_from(target.height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(4))
                .context("Vulkan capture dimensions overflow")?;
            let buffer = self.create_readback_buffer(byte_len)?;
            let to_transfer = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(target_image)
                .subresource_range(color_range());
            unsafe {
                self.device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_transfer],
                );
                self.device.cmd_copy_image_to_buffer(
                    command,
                    target_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.buffer,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1),
                        )
                        .image_extent(vk::Extent3D {
                            width: target.width,
                            height: target.height,
                            depth: 1,
                        })],
                );
            }
            Some(PendingCapture {
                id,
                buffer,
                byte_len,
                width: target.width,
                height: target.height,
                fourcc: target.fourcc,
            })
        } else {
            None
        };

        let release = vk::ImageMemoryBarrier::default()
            .src_access_mask(if capture.is_some() {
                vk::AccessFlags::TRANSFER_READ
            } else {
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            })
            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
            .old_layout(if capture.is_some() {
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL
            } else {
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
            })
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(self.queue_family)
            .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .image(target_image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                if capture.is_some() {
                    vk::PipelineStageFlags::TRANSFER
                } else {
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                },
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[release],
            );
            self.device.end_command_buffer(command)?;
        }
        let rendered_target = self.targets.get_mut(&target_key).unwrap();
        rendered_target.initialized = true;
        rendered_target.last_frame = stream_frame;

        let mut export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
        let semaphore = unsafe {
            self.device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut export),
                None,
            )
        }?;
        let fence = unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }?;
        let signal = [semaphore];
        let commands = [command];
        unsafe {
            self.device.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .command_buffers(&commands)
                    .signal_semaphores(&signal)],
                fence,
            )
        }?;
        let raw_fd = unsafe {
            self.external_semaphore_fd.get_semaphore_fd(
                &vk::SemaphoreGetFdInfoKHR::default()
                    .semaphore(semaphore)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD),
            )
        }?;
        let fence_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let retentions = surfaces
            .iter()
            .filter_map(|surface| surface.retention.clone())
            .collect();
        self.pending.push(PendingFrame {
            submitted_at: Instant::now(),
            fence,
            command,
            semaphore,
            framebuffer,
            retired_textures,
            staging,
            capture,
            _retentions: retentions,
        });
        Ok(AshDrmSubmission { fence_fd })
    }

    fn ensure_pipelines(&mut self, format: vk::Format) -> Result<()> {
        if self.pipelines.contains_key(&format) {
            return Ok(());
        }
        let attachment = [vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let color_ref = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let render_pass = unsafe {
            self.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachment)
                    .subpasses(&subpass),
                None,
            )
        }?;
        let solid =
            self.create_pipeline(render_pass, self.solid_layout, SOLID_SHADER, false, false)?;
        let texture = self.create_pipeline(
            render_pass,
            self.texture_layout,
            TEXTURE_SHADER,
            true,
            false,
        )?;
        let mesh =
            self.create_pipeline(render_pass, self.texture_layout, MESH_SHADER, true, true)?;
        self.pipelines.insert(
            format,
            Pipelines {
                render_pass,
                solid,
                texture,
                mesh,
            },
        );
        Ok(())
    }

    fn create_pipeline(
        &self,
        render_pass: vk::RenderPass,
        layout: vk::PipelineLayout,
        source: &str,
        blend: bool,
        mesh_input: bool,
    ) -> Result<vk::Pipeline> {
        let vertex_words = compile_shader(source, naga::ShaderStage::Vertex, "vs_main")?;
        let fragment_words = compile_shader(source, naga::ShaderStage::Fragment, "fs_main")?;
        let vertex = unsafe {
            self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&vertex_words),
                None,
            )
        }?;
        let fragment = unsafe {
            self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&fragment_words),
                None,
            )
        }?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex)
                .name(c"vs_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment)
                .name(c"fs_main"),
        ];
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(blend)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let vertex_bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<MeshVertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let vertex_attributes = [
            vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 8,
            },
            vk::VertexInputAttributeDescription {
                location: 2,
                binding: 0,
                format: vk::Format::R8G8B8A8_UNORM,
                offset: 16,
            },
        ];
        let vertex_input = if mesh_input {
            vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&vertex_bindings)
                .vertex_attribute_descriptions(&vertex_attributes)
        } else {
            vk::PipelineVertexInputStateCreateInfo::default()
        };
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
            .line_width(1.0)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0);
        let result = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        }
        .map_err(|(_, error)| error)
        .context("create raw Vulkan composition pipeline");
        unsafe {
            self.device.destroy_shader_module(vertex, None);
            self.device.destroy_shader_module(fragment, None);
        }
        Ok(result?[0])
    }

    #[allow(clippy::too_many_arguments)]
    fn record_scene_pass(
        &self,
        command: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
        format: vk::Format,
        render_area: vk::Rect2D,
        width: u32,
        height: u32,
        background: &[SolidQuad],
        surfaces: &[TextureQuad],
        textures: &[Option<DrawTexture>],
        overlay: &[SolidQuad],
        overlay_after_surface: usize,
        foreground: &[SolidQuad],
        foreground_after_surface: usize,
        meshes: &[TexturedMesh],
        mesh_draws: &[Option<DrawTexture>],
        prepared_meshes: &[Option<PreparedMesh>],
        mesh_before_surface: usize,
    ) {
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.015, 0.025, 0.055, 1.0],
            },
        }];
        unsafe {
            self.device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.pipelines[&format].render_pass)
                    .framebuffer(framebuffer)
                    .render_area(render_area)
                    .clear_values(&clear),
                vk::SubpassContents::INLINE,
            );
            self.device.cmd_set_viewport(
                command,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(command, 0, &[render_area]);
        }
        self.draw_solids(command, format, background, width, height);
        for (index, (surface, texture)) in surfaces.iter().zip(textures).enumerate() {
            if index == overlay_after_surface {
                self.draw_solids(command, format, overlay, width, height);
            }
            if index == foreground_after_surface {
                self.draw_solids(command, format, foreground, width, height);
            }
            if index == mesh_before_surface {
                self.draw_meshes(
                    command,
                    format,
                    meshes,
                    mesh_draws,
                    prepared_meshes,
                    width,
                    height,
                    render_area,
                );
            }
            if let Some(texture) = texture {
                self.draw_texture(command, format, surface, texture, width, height);
            }
        }
        if overlay_after_surface >= surfaces.len() {
            self.draw_solids(command, format, overlay, width, height);
        }
        if foreground_after_surface >= surfaces.len() {
            self.draw_solids(command, format, foreground, width, height);
        }
        if mesh_before_surface >= surfaces.len() {
            self.draw_meshes(
                command,
                format,
                meshes,
                mesh_draws,
                prepared_meshes,
                width,
                height,
                render_area,
            );
        }
        unsafe { self.device.cmd_end_render_pass(command) };
    }

    fn draw_solids(
        &self,
        command: vk::CommandBuffer,
        format: vk::Format,
        solids: &[SolidQuad],
        width: u32,
        height: u32,
    ) {
        unsafe {
            self.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines[&format].solid,
            )
        };
        for solid in solids {
            let rect = ndc_rect(solid.destination, width, height);
            let mut push = [0.0f32; 8];
            push[..4].copy_from_slice(&rect);
            push[4..].copy_from_slice(&solid.color);
            unsafe {
                self.device.cmd_push_constants(
                    command,
                    self.solid_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    as_bytes(&push),
                );
                self.device.cmd_draw(command, 6, 1, 0, 0);
            }
        }
    }

    fn draw_texture(
        &self,
        command: vk::CommandBuffer,
        format: vk::Format,
        surface: &TextureQuad,
        texture: &DrawTexture,
        width: u32,
        height: u32,
    ) {
        let uvs = transformed_uv(surface.source_uv, surface.transform);
        let mut push = [0.0f32; 16];
        push[..4].copy_from_slice(&ndc_rect(surface.destination, width, height));
        push[4..8].copy_from_slice(&[uvs[0][0], uvs[0][1], uvs[1][0], uvs[1][1]]);
        push[8..12].copy_from_slice(&[uvs[2][0], uvs[2][1], uvs[3][0], uvs[3][1]]);
        push[12..].copy_from_slice(&surface.tint);
        unsafe {
            self.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines[&format].texture,
            );
            self.device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.texture_layout,
                0,
                &[texture.descriptor_set],
                &[],
            );
            self.device.cmd_push_constants(
                command,
                self.texture_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                as_bytes(&push),
            );
            self.device.cmd_draw(command, 6, 1, 0, 0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_meshes(
        &self,
        command: vk::CommandBuffer,
        format: vk::Format,
        meshes: &[TexturedMesh],
        textures: &[Option<DrawTexture>],
        prepared: &[Option<PreparedMesh>],
        width: u32,
        height: u32,
        damage_clip: vk::Rect2D,
    ) {
        let screen = [2.0 / width as f32, -2.0 / height as f32, -1.0, 1.0];
        unsafe {
            self.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines[&format].mesh,
            );
            self.device.cmd_push_constants(
                command,
                self.texture_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                as_bytes(&screen),
            );
        }
        for ((mesh, texture), prepared) in meshes.iter().zip(textures).zip(prepared) {
            let (Some(texture), Some(prepared)) = (texture, prepared) else {
                continue;
            };
            let [x, y, w, h] = mesh.clip_rect;
            let damage_x0 = damage_clip.offset.x;
            let damage_y0 = damage_clip.offset.y;
            let damage_x1 = damage_x0.saturating_add(damage_clip.extent.width as i32);
            let damage_y1 = damage_y0.saturating_add(damage_clip.extent.height as i32);
            let x0 = x.clamp(damage_x0, damage_x1.min(width as i32));
            let y0 = y.clamp(damage_y0, damage_y1.min(height as i32));
            let x1 = x.saturating_add(w).clamp(x0, damage_x1.min(width as i32));
            let y1 = y.saturating_add(h).clamp(y0, damage_y1.min(height as i32));
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let scissor = vk::Rect2D {
                offset: vk::Offset2D { x: x0, y: y0 },
                extent: vk::Extent2D {
                    width: (x1 - x0) as u32,
                    height: (y1 - y0) as u32,
                },
            };
            unsafe {
                self.device.cmd_set_scissor(command, 0, &[scissor]);
                self.device.cmd_bind_descriptor_sets(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.texture_layout,
                    0,
                    &[texture.descriptor_set],
                    &[],
                );
                self.device
                    .cmd_bind_vertex_buffers(command, 0, &[prepared.vertex], &[0]);
                self.device.cmd_bind_index_buffer(
                    command,
                    prepared.index,
                    0,
                    vk::IndexType::UINT32,
                );
                self.device
                    .cmd_draw_indexed(command, prepared.index_count, 1, 0, 0, 0);
            }
        }
        unsafe { self.device.cmd_set_scissor(command, 0, &[damage_clip]) }
    }

    fn prepare_texture(
        &self,
        command: vk::CommandBuffer,
        surface: &TextureQuad,
        staging: &mut Vec<BufferResource>,
    ) -> Result<Option<TextureResource>> {
        if surface.width == 0
            || surface.height == 0
            || surface.destination[2] <= 0
            || surface.destination[3] <= 0
        {
            return Ok(None);
        }
        #[cfg(unix)]
        if let Some(dmabuf) = &surface.dmabuf {
            let target = DrmRenderTarget {
                planes: vec![dmabuf.fd.clone()],
                offsets: vec![dmabuf.offset as u32],
                strides: vec![surface.stride],
                fourcc: texture_fourcc(surface.format),
                modifier: dmabuf.modifier,
                width: surface.width,
                height: surface.height,
            };
            let image = self.import_dmabuf_image(
                &target,
                texture_vk_format(surface.format),
                vk::ImageUsageFlags::SAMPLED,
            )?;
            return Ok(Some(self.texture_resource(image, true)?));
        }
        let required = usize::try_from(surface.stride)
            .ok()
            .and_then(|stride| stride.checked_mul(surface.height as usize));
        if required.is_none_or(|required| surface.pixels.len() < required) {
            return Ok(None);
        }
        let format = texture_vk_format(surface.format);
        let image = self.create_owned_image(
            surface.width,
            surface.height,
            format,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        )?;
        let stage = self.create_staging(&surface.pixels)?;
        let to_copy = vk::ImageMemoryBarrier::default()
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(image.image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_copy],
            );
            self.device.cmd_copy_buffer_to_image(
                command,
                stage.buffer,
                image.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .buffer_row_length(surface.stride / 4)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: surface.width,
                        height: surface.height,
                        depth: 1,
                    })],
            );
            let sampled = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image(image.image)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[sampled],
            );
        }
        staging.push(stage);
        Ok(Some(self.texture_resource(image, false)?))
    }

    fn texture_resource(&self, image: ImageResource, foreign: bool) -> Result<TextureResource> {
        let layouts = [self.descriptor_layout];
        let descriptor_set = unsafe {
            self.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&layouts),
            )
        }?[0];
        let image_info = [vk::DescriptorImageInfo::default()
            .image_view(image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let sampler_info = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        unsafe {
            self.device.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(&image_info),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::SAMPLER)
                        .image_info(&sampler_info),
                ],
                &[],
            )
        };
        Ok(TextureResource {
            image,
            descriptor_set,
            foreign,
        })
    }

    fn import_dmabuf_image(
        &self,
        target: &DrmRenderTarget,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Result<ImageResource> {
        ensure!(!target.planes.is_empty(), "DMA-BUF has no planes");
        ensure!(
            target.modifier != u64::MAX,
            "implicit DRM modifiers cannot be imported safely"
        );
        ensure!(
            target.planes.len() == target.offsets.len()
                && target.planes.len() == target.strides.len(),
            "DMA-BUF plane metadata mismatch"
        );
        ensure!(
            target.planes.len() == 1,
            "multi-plane Vulkan DMA-BUF import is not implemented"
        );
        let layouts = [vk::SubresourceLayout {
            offset: u64::from(target.offsets[0]),
            row_pitch: u64::from(target.strides[0]),
            ..Default::default()
        }];
        let mut modifier = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(target.modifier)
            .plane_layouts(&layouts);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let image = unsafe {
            self.device.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: target.width,
                        height: target.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .push_next(&mut modifier)
                    .push_next(&mut external),
                None,
            )
        }
        .context("create Vulkan DMA-BUF image")?;
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let owned_fd = target.planes[0]
            .try_clone()
            .context("duplicate DMA-BUF for Vulkan import")?;
        let raw_fd = owned_fd.into_raw_fd();
        let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
        if let Err(error) = unsafe {
            self.external_memory_fd.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                raw_fd,
                &mut fd_properties,
            )
        } {
            unsafe {
                drop(OwnedFd::from_raw_fd(raw_fd));
                self.device.destroy_image(image, None);
            }
            return Err(error).context("query DMA-BUF Vulkan memory types");
        }
        let memory_type = self.memory_type(
            requirements.memory_type_bits & fd_properties.memory_type_bits,
            vk::MemoryPropertyFlags::empty(),
        );
        let memory_type = match memory_type {
            Ok(memory_type) => memory_type,
            Err(error) => {
                unsafe {
                    drop(OwnedFd::from_raw_fd(raw_fd));
                    self.device.destroy_image(image, None);
                }
                return Err(error);
            }
        };
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(raw_fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocation = unsafe {
            self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type)
                    .push_next(&mut import)
                    .push_next(&mut dedicated),
                None,
            )
        };
        let memory = match allocation {
            Ok(memory) => memory,
            Err(error) => {
                unsafe {
                    drop(OwnedFd::from_raw_fd(raw_fd));
                    self.device.destroy_image(image, None);
                }
                return Err(error).context("import DMA-BUF memory");
            }
        };
        unsafe { self.device.bind_image_memory(image, memory, 0) }
            .context("bind imported DMA-BUF image")?;
        let view = unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )
        }?;
        Ok(ImageResource {
            image,
            memory,
            view,
        })
    }

    fn create_owned_image(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Result<ImageResource> {
        let image = unsafe {
            self.device.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
        }?;
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type = self.memory_type(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let memory = unsafe {
            self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )
        }?;
        unsafe { self.device.bind_image_memory(image, memory, 0) }?;
        let view = unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )
        }?;
        Ok(ImageResource {
            image,
            memory,
            view,
        })
    }

    fn create_staging(&self, bytes: &[u8]) -> Result<BufferResource> {
        self.create_host_buffer(bytes, vk::BufferUsageFlags::TRANSFER_SRC)
    }

    fn create_host_buffer(
        &self,
        bytes: &[u8],
        usage: vk::BufferUsageFlags,
    ) -> Result<BufferResource> {
        let buffer = unsafe {
            self.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(bytes.len() as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
        }?;
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory_type = self.memory_type(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let memory = unsafe {
            self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )
        }?;
        unsafe {
            self.device.bind_buffer_memory(buffer, memory, 0)?;
            let ptr = self.device.map_memory(
                memory,
                0,
                bytes.len() as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), bytes.len());
            self.device.unmap_memory(memory);
        }
        Ok(BufferResource { buffer, memory })
    }

    fn create_readback_buffer(&self, byte_len: usize) -> Result<BufferResource> {
        ensure!(byte_len > 0, "zero-sized Vulkan readback buffer");
        let buffer = unsafe {
            self.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(byte_len as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
        }?;
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory_type = self.memory_type(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let memory = unsafe {
            self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )
        }?;
        unsafe { self.device.bind_buffer_memory(buffer, memory, 0) }?;
        Ok(BufferResource { buffer, memory })
    }

    fn read_completed_capture(&self, capture: &PendingCapture) -> Result<AshDrmCapture> {
        let ptr = unsafe {
            self.device.map_memory(
                capture.buffer.memory,
                0,
                capture.byte_len as u64,
                vk::MemoryMapFlags::empty(),
            )
        }?;
        let pixels =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), capture.byte_len).to_vec() };
        unsafe { self.device.unmap_memory(capture.buffer.memory) };
        Ok(AshDrmCapture {
            id: capture.id,
            width: capture.width,
            height: capture.height,
            fourcc: capture.fourcc,
            pixels,
        })
    }

    fn destroy_buffer(&self, buffer: BufferResource) {
        unsafe {
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        (0..self.memory_properties.memory_type_count)
            .find(|index| {
                bits & (1 << index) != 0
                    && self.memory_properties.memory_types[*index as usize]
                        .property_flags
                        .contains(flags)
            })
            .ok_or_else(|| anyhow!("no compatible Vulkan memory type for {flags:?}"))
    }

    fn destroy_pending(&self, frame: PendingFrame) {
        unsafe {
            if let Some(capture) = frame.capture {
                self.device.destroy_buffer(capture.buffer.buffer, None);
                self.device.free_memory(capture.buffer.memory, None);
            }
            for texture in frame.retired_textures {
                if texture.image.image == vk::Image::null() {
                    continue;
                }
                if texture.foreign {
                    // The imported fd is independent; the Wayland buffer itself remains retained by the scene.
                }
                self.device
                    .free_descriptor_sets(self.descriptor_pool, &[texture.descriptor_set])
                    .ok();
                self.destroy_image(texture.image);
            }
            for buffer in frame.staging {
                self.device.destroy_buffer(buffer.buffer, None);
                self.device.free_memory(buffer.memory, None);
            }
            self.device.destroy_framebuffer(frame.framebuffer, None);
            self.device.destroy_semaphore(frame.semaphore, None);
            self.device.destroy_fence(frame.fence, None);
            self.device
                .free_command_buffers(self.command_pool, &[frame.command]);
        }
    }

    unsafe fn destroy_image(&self, image: ImageResource) {
        unsafe {
            self.device.destroy_image_view(image.view, None);
            self.device.destroy_image(image.image, None);
            self.device.free_memory(image.memory, None);
        }
    }
}

impl TextureResource {
    fn null() -> Self {
        Self {
            image: ImageResource {
                image: vk::Image::null(),
                memory: vk::DeviceMemory::null(),
                view: vk::ImageView::null(),
            },
            descriptor_set: vk::DescriptorSet::null(),
            foreign: false,
        }
    }
}

impl Drop for AshDrmRenderer {
    fn drop(&mut self) {
        if self.abandon_on_drop {
            // The process is exiting. Let the kernel close the Vulkan device
            // file descriptors rather than risking an unbounded driver wait.
            return;
        }
        unsafe {
            self.device.device_wait_idle().ok();
            for frame in self.pending.drain(..) {
                // Inline cleanup avoids borrowing all of self during drain.
                for texture in frame.retired_textures {
                    if texture.image.image != vk::Image::null() {
                        self.device
                            .free_descriptor_sets(self.descriptor_pool, &[texture.descriptor_set])
                            .ok();
                        self.device.destroy_image_view(texture.image.view, None);
                        self.device.destroy_image(texture.image.image, None);
                        self.device.free_memory(texture.image.memory, None);
                    }
                }
                for buffer in frame.staging {
                    self.device.destroy_buffer(buffer.buffer, None);
                    self.device.free_memory(buffer.memory, None);
                }
                if let Some(capture) = frame.capture {
                    self.device.destroy_buffer(capture.buffer.buffer, None);
                    self.device.free_memory(capture.buffer.memory, None);
                }
                self.device.destroy_framebuffer(frame.framebuffer, None);
                self.device.destroy_semaphore(frame.semaphore, None);
                self.device.destroy_fence(frame.fence, None);
                self.device
                    .free_command_buffers(self.command_pool, &[frame.command]);
            }
            for (_, target) in self.targets.drain() {
                self.device.destroy_image_view(target.image.view, None);
                self.device.destroy_image(target.image.image, None);
                self.device.free_memory(target.image.memory, None);
            }
            for (_, cached) in self.texture_cache.drain() {
                self.device
                    .free_descriptor_sets(self.descriptor_pool, &[cached.resource.descriptor_set])
                    .ok();
                self.device
                    .destroy_image_view(cached.resource.image.view, None);
                self.device.destroy_image(cached.resource.image.image, None);
                self.device.free_memory(cached.resource.image.memory, None);
            }
            for (_, pipeline) in self.pipelines.drain() {
                self.device.destroy_pipeline(pipeline.solid, None);
                self.device.destroy_pipeline(pipeline.texture, None);
                self.device.destroy_pipeline(pipeline.mesh, None);
                self.device.destroy_render_pass(pipeline.render_pass, None);
            }
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_pipeline_layout(self.solid_layout, None);
            self.device
                .destroy_pipeline_layout(self.texture_layout, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn compile_shader(source: &str, stage: naga::ShaderStage, entry_point: &str) -> Result<Vec<u32>> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| anyhow!(error.emit_to_string(source)))?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .context("validate Vulkan composition shader")?;
    naga::back::spv::write_vec(
        &module,
        &info,
        &naga::back::spv::Options::default(),
        Some(&naga::back::spv::PipelineOptions {
            shader_stage: stage,
            entry_point: entry_point.into(),
        }),
    )
    .context("compile Vulkan composition shader")
}

fn vk_format(fourcc: u32) -> Result<vk::Format> {
    match fourcc {
        XRGB8888 | ARGB8888 => Ok(vk::Format::B8G8R8A8_UNORM),
        XBGR8888 | ABGR8888 => Ok(vk::Format::R8G8B8A8_UNORM),
        _ => bail!("unsupported DRM target fourcc 0x{fourcc:08x}"),
    }
}

fn texture_vk_format(format: FramePixelFormat) -> vk::Format {
    match format {
        // Preserve encoded SDR samples. The KMS attachment is UNORM and the
        // current compositor pass intentionally matches the GLES encoded-SDR
        // path; a future linear-light pass must add an explicit output encode.
        FramePixelFormat::Bgra8Srgb => vk::Format::B8G8R8A8_UNORM,
        FramePixelFormat::Rgba8Srgb => vk::Format::R8G8B8A8_UNORM,
    }
}

fn texture_fourcc(format: FramePixelFormat) -> u32 {
    match format {
        FramePixelFormat::Bgra8Srgb => ARGB8888,
        FramePixelFormat::Rgba8Srgb => ABGR8888,
    }
}

fn target_key(target: &DrmRenderTarget) -> Result<TargetKey> {
    let stat = unsafe {
        let mut value = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if libc::fstat(target.planes[0].as_raw_fd(), value.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error()).context("identify GBM DMA-BUF");
        }
        value.assume_init()
    };
    Ok(TargetKey {
        device: stat.st_dev,
        inode: stat.st_ino,
        modifier: target.modifier,
        width: target.width,
        height: target.height,
        fourcc: target.fourcc,
    })
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

fn ndc_rect(rect: [i32; 4], width: u32, height: u32) -> [f32; 4] {
    let [x, y, w, h] = rect;
    [
        x as f32 / width as f32 * 2.0 - 1.0,
        1.0 - y as f32 / height as f32 * 2.0,
        w as f32 / width as f32 * 2.0,
        -(h as f32 / height as f32 * 2.0),
    ]
}

fn effective_damage_regions(
    initialized: bool,
    last_target_frame: u64,
    history: &DamageHistory,
    current: &[DamageRect],
    width: u32,
    height: u32,
) -> Vec<vk::Rect2D> {
    let full = || {
        vec![vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D { width, height },
        }]
    };
    if !initialized
        || history
            .front()
            .is_some_and(|(oldest, _)| *oldest > last_target_frame.saturating_add(1))
    {
        return full();
    }

    let mut regions = history
        .iter()
        .filter(|(frame, _)| *frame > last_target_frame)
        .flat_map(|(_, regions)| regions.iter().copied())
        .chain(current.iter().copied())
        .filter_map(|region| clip_damage_region(region, width, height))
        .collect::<Vec<_>>();
    merge_damage_regions(&mut regions);
    if regions.len() > 16 {
        let x0 = regions
            .iter()
            .map(|region| region.offset.x)
            .min()
            .unwrap_or(0);
        let y0 = regions
            .iter()
            .map(|region| region.offset.y)
            .min()
            .unwrap_or(0);
        let x1 = regions
            .iter()
            .map(|region| region.offset.x + region.extent.width as i32)
            .max()
            .unwrap_or(width as i32);
        let y1 = regions
            .iter()
            .map(|region| region.offset.y + region.extent.height as i32)
            .max()
            .unwrap_or(height as i32);
        regions = vec![vk::Rect2D {
            offset: vk::Offset2D { x: x0, y: y0 },
            extent: vk::Extent2D {
                width: (x1 - x0) as u32,
                height: (y1 - y0) as u32,
            },
        }];
    }
    regions
}

fn clip_damage_region(region: [i32; 4], width: u32, height: u32) -> Option<vk::Rect2D> {
    let [x, y, w, h] = region;
    let x0 = x.clamp(0, width as i32);
    let y0 = y.clamp(0, height as i32);
    let x1 = x.saturating_add(w).clamp(x0, width as i32);
    let y1 = y.saturating_add(h).clamp(y0, height as i32);
    (x1 > x0 && y1 > y0).then_some(vk::Rect2D {
        offset: vk::Offset2D { x: x0, y: y0 },
        extent: vk::Extent2D {
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        },
    })
}

fn merge_damage_regions(regions: &mut Vec<vk::Rect2D>) {
    let mut index = 0;
    while index < regions.len() {
        let mut other = index + 1;
        while other < regions.len() {
            let a = regions[index];
            let b = regions[other];
            let ax1 = a.offset.x + a.extent.width as i32;
            let ay1 = a.offset.y + a.extent.height as i32;
            let bx1 = b.offset.x + b.extent.width as i32;
            let by1 = b.offset.y + b.extent.height as i32;
            if a.offset.x <= bx1 && b.offset.x <= ax1 && a.offset.y <= by1 && b.offset.y <= ay1 {
                let x0 = a.offset.x.min(b.offset.x);
                let y0 = a.offset.y.min(b.offset.y);
                let x1 = ax1.max(bx1);
                let y1 = ay1.max(by1);
                regions[index] = vk::Rect2D {
                    offset: vk::Offset2D { x: x0, y: y0 },
                    extent: vk::Extent2D {
                        width: (x1 - x0) as u32,
                        height: (y1 - y0) as u32,
                    },
                };
                let _ = regions.swap_remove(other);
                other = index + 1;
            } else {
                other += 1;
            }
        }
        index += 1;
    }
}

fn transformed_uv(source: [f32; 4], transform: FrameTransform) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = source;
    let (tl, tr, br, bl) = ([u0, v0], [u1, v0], [u1, v1], [u0, v1]);
    match transform {
        FrameTransform::Normal => [tl, tr, br, bl],
        FrameTransform::Rotate90 => [bl, tl, tr, br],
        FrameTransform::Rotate180 => [br, bl, tl, tr],
        FrameTransform::Rotate270 => [tr, br, bl, tl],
        FrameTransform::Flipped => [tr, tl, bl, br],
        FrameTransform::Flipped90 => [tl, bl, br, tr],
        FrameTransform::Flipped180 => [bl, br, tr, tl],
        FrameTransform::Flipped270 => [br, tr, tl, bl],
    }
}

fn as_bytes<T>(value: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(value.as_ptr().cast(), std::mem::size_of_val(value)) }
}

#[cfg(test)]
mod tests {
    use super::{
        compile_shader, effective_damage_regions, ndc_rect, texture_vk_format, transformed_uv,
        AshDrmCapture, ARGB8888, MESH_SHADER, SOLID_SHADER, TEXTURE_SHADER,
    };
    use crate::{FramePixelFormat, FrameTransform};
    use std::collections::VecDeque;

    #[test]
    fn raw_vulkan_shaders_compile_to_spirv() {
        for source in [SOLID_SHADER, TEXTURE_SHADER, MESH_SHADER] {
            let vertex = compile_shader(source, naga::ShaderStage::Vertex, "vs_main").unwrap();
            let fragment = compile_shader(source, naga::ShaderStage::Fragment, "fs_main").unwrap();
            assert_eq!(vertex.first().copied(), Some(0x0723_0203));
            assert_eq!(fragment.first().copied(), Some(0x0723_0203));
        }
    }

    #[test]
    fn output_rect_is_converted_to_vulkan_clip_space() {
        assert_eq!(ndc_rect([0, 0, 100, 50], 100, 50), [-1.0, 1.0, 2.0, -2.0]);
    }

    #[test]
    fn encoded_sdr_textures_are_not_decoded_without_an_output_encode() {
        assert_eq!(
            texture_vk_format(FramePixelFormat::Bgra8Srgb),
            ash::vk::Format::B8G8R8A8_UNORM
        );
        assert_eq!(
            texture_vk_format(FramePixelFormat::Rgba8Srgb),
            ash::vk::Format::R8G8B8A8_UNORM
        );
    }

    #[test]
    fn rotated_texture_coordinates_keep_all_corners() {
        assert_eq!(
            transformed_uv([0.0, 0.0, 1.0, 1.0], FrameTransform::Rotate90),
            [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]
        );
    }

    #[test]
    fn bgra_scanout_capture_is_normalized_to_rgba() {
        let capture = AshDrmCapture {
            id: 7,
            width: 1,
            height: 1,
            fourcc: ARGB8888,
            pixels: vec![10, 20, 30, 99],
        };
        assert_eq!(capture.into_rgba8().unwrap(), vec![30, 20, 10, 255]);
    }

    #[test]
    fn damage_history_repairs_a_swapchain_buffer_that_missed_frames() {
        let history = VecDeque::from([(2, vec![[10, 10, 20, 20]]), (3, vec![[40, 40, 10, 10]])]);
        let regions = effective_damage_regions(true, 1, &history, &[[80, 80, 10, 10]], 100, 100);
        assert_eq!(regions.len(), 3);
        assert!(regions.iter().any(|region| region.offset.x == 10));
        assert!(regions.iter().any(|region| region.offset.x == 40));
        assert!(regions.iter().any(|region| region.offset.x == 80));
    }

    #[test]
    fn missing_damage_history_forces_a_full_repaint() {
        let history = VecDeque::from([(9, vec![[10, 10, 20, 20]])]);
        let regions = effective_damage_regions(true, 2, &history, &[[80, 80, 10, 10]], 100, 100);
        assert_eq!(regions[0].offset.x, 0);
        assert_eq!(regions[0].offset.y, 0);
        assert_eq!(regions[0].extent.width, 100);
        assert_eq!(regions[0].extent.height, 100);
    }
}

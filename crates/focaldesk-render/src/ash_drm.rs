//! Raw Vulkan renderer for compositor-owned DRM/GBM scanout buffers.
//!
//! This module deliberately has no Vulkan WSI surface or swapchain. KMS owns
//! presentation; Vulkan only imports DMA-BUF images, records rendering, and
//! exports a sync-file for the atomic commit's `IN_FENCE_FD`.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fmt;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ash::{ext, khr, vk, Entry};

use crate::{
    DrmRenderTarget, FramePixelFormat, FrameRetention, FrameTransform, RendererInfo, SolidQuad,
    TextureQuad,
};

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
const XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
const ABGR8888: u32 = u32::from_le_bytes(*b"AB24");

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

struct BufferResource {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
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
}

struct PendingFrame {
    fence: vk::Fence,
    command: vk::CommandBuffer,
    semaphore: vk::Semaphore,
    framebuffer: vk::Framebuffer,
    retired_textures: Vec<TextureResource>,
    staging: Vec<BufferResource>,
    _retentions: Vec<FrameRetention>,
}

struct Pipelines {
    render_pass: vk::RenderPass,
    solid: vk::Pipeline,
    texture: vk::Pipeline,
}

/// Completed raw-Vulkan submission and the sync-file KMS must wait on.
#[derive(Debug)]
pub struct AshDrmSubmission {
    pub fence_fd: OwnedFd,
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
        self.formats_for_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
    }

    /// Formats accepted when importing client DMA-BUFs for sampling.
    pub fn sample_formats(&self) -> Vec<(u32, u64)> {
        self.formats_for_usage(vk::ImageUsageFlags::SAMPLED)
    }

    fn formats_for_usage(&self, usage: vk::ImageUsageFlags) -> Vec<(u32, u64)> {
        let mut formats = Vec::new();
        for (fourcc, render_format, sample_format) in [
            (
                XRGB8888,
                vk::Format::B8G8R8A8_UNORM,
                vk::Format::B8G8R8A8_SRGB,
            ),
            (
                ARGB8888,
                vk::Format::B8G8R8A8_UNORM,
                vk::Format::B8G8R8A8_SRGB,
            ),
            (
                XBGR8888,
                vk::Format::R8G8B8A8_UNORM,
                vk::Format::R8G8B8A8_SRGB,
            ),
            (
                ABGR8888,
                vk::Format::R8G8B8A8_UNORM,
                vk::Format::R8G8B8A8_SRGB,
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
                let required_feature = if usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
                    vk::FormatFeatureFlags::COLOR_ATTACHMENT
                } else {
                    vk::FormatFeatureFlags::SAMPLED_IMAGE
                };
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
            let reached = unsafe { self.device.get_fence_status(self.pending[index].fence) }?;
            if reached {
                let frame = self.pending.swap_remove(index);
                self.destroy_pending(frame);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    pub fn render(
        &mut self,
        target: &DrmRenderTarget,
        background: &[SolidQuad],
        surfaces: &[TextureQuad],
        overlay: &[SolidQuad],
        overlay_after_surface: usize,
        _damage: &[[i32; 4]],
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
            let image =
                self.import_dmabuf_image(target, format, vk::ImageUsageFlags::COLOR_ATTACHMENT)?;
            self.targets.insert(
                target_key,
                ImportedTarget {
                    image,
                    initialized: false,
                },
            );
        }
        let target_image = self.targets[&target_key].image.image;
        let target_view = self.targets[&target_key].image.view;
        let initialized = self.targets[&target_key].initialized;

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

        self.frame_no = self.frame_no.wrapping_add(1);
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
        let active_keys = surfaces
            .iter()
            .map(|surface| surface.cache_key)
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
        // The GBM swapchain can return a different target on each frame. Until
        // retained per-buffer damage history is implemented, redraw the whole
        // frame so untouched regions never contain stale pixels.
        let render_area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: target.width,
                height: target.height,
            },
        };
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
                    width: target.width as f32,
                    height: target.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(command, 0, &[render_area]);
        }
        self.draw_solids(command, format, background, target.width, target.height);
        for (index, (surface, texture)) in surfaces.iter().zip(&textures).enumerate() {
            if index == overlay_after_surface {
                self.draw_solids(command, format, overlay, target.width, target.height);
            }
            if let Some(texture) = texture {
                self.draw_texture(
                    command,
                    format,
                    surface,
                    texture,
                    target.width,
                    target.height,
                );
            }
        }
        if overlay_after_surface >= surfaces.len() {
            self.draw_solids(command, format, overlay, target.width, target.height);
        }
        unsafe { self.device.cmd_end_render_pass(command) };

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

        let release = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(self.queue_family)
            .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .image(target_image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[release],
            );
            self.device.end_command_buffer(command)?;
        }
        self.targets.get_mut(&target_key).unwrap().initialized = true;

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
            fence,
            command,
            semaphore,
            framebuffer,
            retired_textures,
            staging,
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
        let solid = self.create_pipeline(render_pass, self.solid_layout, SOLID_SHADER, false)?;
        let texture =
            self.create_pipeline(render_pass, self.texture_layout, TEXTURE_SHADER, true)?;
        self.pipelines.insert(
            format,
            Pipelines {
                render_pass,
                solid,
                texture,
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
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
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
        let buffer = unsafe {
            self.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(bytes.len() as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
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
        FramePixelFormat::Bgra8Srgb => vk::Format::B8G8R8A8_SRGB,
        FramePixelFormat::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
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
    use super::{compile_shader, ndc_rect, transformed_uv, SOLID_SHADER, TEXTURE_SHADER};
    use crate::FrameTransform;

    #[test]
    fn raw_vulkan_shaders_compile_to_spirv() {
        for source in [SOLID_SHADER, TEXTURE_SHADER] {
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
    fn rotated_texture_coordinates_keep_all_corners() {
        assert_eq!(
            transformed_uv([0.0, 0.0, 1.0, 1.0], FrameTransform::Rotate90),
            [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]
        );
    }
}

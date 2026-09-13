use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use wgpu::util::{BufferInitDescriptor, DeviceExt};
use wgpu::{
    Backends, BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingResource, BindingType, BlendState, BufferUsages, Color, ColorTargetState, ColorWrites,
    CommandEncoderDescriptor, CurrentSurfaceTexture, DeviceDescriptor, Extent3d, Features,
    FilterMode, FragmentState, InstanceDescriptor, LoadOp, MultisampleState, Operations,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PowerPreference, PrimitiveState,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
    RequestAdapterOptions, SamplerBindingType, SamplerDescriptor, ShaderModuleDescriptor,
    ShaderSource, ShaderStages, StoreOp, Surface, SurfaceConfiguration, TexelCopyBufferLayout,
    TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, TextureUses, TextureViewDescriptor, TextureViewDimension,
    VertexBufferLayout, VertexState, VertexStepMode,
};
use winit::window::Window;

use crate::{
    FramePixelFormat, FrameTransform, GraphicsApi, PresentRenderer, PresentResult, RendererInfo,
    TextureQuad,
};

fn surface_extent(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

fn transformed_uv(source: [f32; 4], transform: FrameTransform) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = source;
    let tl = [u0, v0];
    let tr = [u1, v0];
    let br = [u1, v1];
    let bl = [u0, v1];
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const SURFACE_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

struct CachedTexture {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    format: FramePixelFormat,
    external: bool,
    last_used_frame: u64,
}

fn texture_format(format: FramePixelFormat) -> TextureFormat {
    match format {
        FramePixelFormat::Bgra8Srgb => TextureFormat::Bgra8UnormSrgb,
        FramePixelFormat::Rgba8Srgb => TextureFormat::Rgba8UnormSrgb,
    }
}

/// Vulkan-only wgpu renderer used to establish the nested presentation path.
pub struct WgpuVulkanRenderer {
    instance: wgpu::Instance,
    window: Arc<Window>,
    surface: Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    probe_pipeline: wgpu::RenderPipeline,
    surface_pipeline: wgpu::RenderPipeline,
    surface_bind_group_layout: wgpu::BindGroupLayout,
    surface_sampler: wgpu::Sampler,
    texture_cache: HashMap<u64, CachedTexture>,
    frame_no: u64,
    dmabuf_imports: u64,
    dmabuf_fallbacks: u64,
    info: RendererInfo,
}

impl WgpuVulkanRenderer {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        pollster::block_on(Self::new_async(window))
    }

    pub fn supports_direct_dmabuf_import(&self) -> bool {
        self.device
            .features()
            .contains(Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
    }

    pub fn dmabuf_import_stats(&self) -> (u64, u64) {
        (self.dmabuf_imports, self.dmabuf_fallbacks)
    }

    async fn new_async(window: Arc<Window>) -> Result<Self> {
        let mut instance_descriptor = InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);
        let surface = instance
            .create_surface(window.clone())
            .context("create Vulkan presentation surface")?;

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("find a Vulkan adapter capable of presenting to the nested window")?;
        let adapter_info = adapter.get_info();
        ensure!(
            adapter_info.backend == wgpu::Backend::Vulkan,
            "wgpu selected {:?}, but the FocalDesk backend requires Vulkan",
            adapter_info.backend
        );

        let dmabuf_feature = Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
        let required_features = adapter.features() & dmabuf_feature;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("focaldesk-wgpu-vulkan-device"),
                required_features,
                ..Default::default()
            })
            .await
            .context("create wgpu Vulkan device")?;

        let size = window.inner_size();
        let (width, height) = surface_extent(size.width, size.height).unwrap_or((1, 1));
        let mut config = surface
            .get_default_config(&adapter, width, height)
            .context("Vulkan adapter cannot configure the nested window surface")?;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);

        let probe_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("focaldesk-wgpu-probe-shader"),
            source: ShaderSource::Wgsl(include_str!("probe.wgsl").into()),
        });
        let probe_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("focaldesk-wgpu-probe-pipeline"),
            layout: None,
            vertex: VertexState {
                module: &probe_shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &probe_shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let surface_bind_group_layout =
            device.create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: Some("focaldesk-shm-surface-layout"),
                entries: &[
                    BindGroupLayoutEntry {
                        binding: 0,
                        visibility: ShaderStages::FRAGMENT,
                        ty: BindingType::Texture {
                            sample_type: TextureSampleType::Float { filterable: true },
                            view_dimension: TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    BindGroupLayoutEntry {
                        binding: 1,
                        visibility: ShaderStages::FRAGMENT,
                        ty: BindingType::Sampler(SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let surface_pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("focaldesk-shm-surface-pipeline-layout"),
            bind_group_layouts: &[Some(&surface_bind_group_layout)],
            immediate_size: 0,
        });
        let surface_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("focaldesk-shm-surface-shader"),
            source: ShaderSource::Wgsl(include_str!("shm_surface.wgsl").into()),
        });
        let surface_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("focaldesk-shm-surface-pipeline"),
            layout: Some(&surface_pipeline_layout),
            vertex: VertexState {
                module: &surface_shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[Some(VertexBufferLayout {
                    array_stride: std::mem::size_of::<SurfaceVertex>() as u64,
                    step_mode: VertexStepMode::Vertex,
                    attributes: &SURFACE_VERTEX_ATTRIBUTES,
                })],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &surface_shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: config.format,
                    blend: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let surface_sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("focaldesk-shm-surface-sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });

        let info = RendererInfo {
            api: GraphicsApi::Vulkan,
            adapter_name: adapter_info.name,
            driver: adapter_info.driver,
            driver_info: adapter_info.driver_info,
            surface_format: format!("{:?}", config.format),
        };

        Ok(Self {
            instance,
            window,
            surface,
            device,
            queue,
            config,
            probe_pipeline,
            surface_pipeline,
            surface_bind_group_layout,
            surface_sampler,
            texture_cache: HashMap::new(),
            frame_no: 0,
            dmabuf_imports: 0,
            dmabuf_fallbacks: 0,
            info,
        })
    }

    fn configure_surface(&self) {
        self.surface.configure(&self.device, &self.config);
    }

    fn recreate_surface(&mut self) -> Result<()> {
        self.surface = self
            .instance
            .create_surface(self.window.clone())
            .context("recreate lost Vulkan presentation surface")?;
        self.configure_surface();
        Ok(())
    }

    #[cfg(unix)]
    fn import_dmabuf_texture(&self, surface: &TextureQuad) -> Result<Option<wgpu::Texture>> {
        use wgpu::hal::{self, api::Vulkan};

        let Some(dmabuf) = surface.dmabuf.as_ref() else {
            return Ok(None);
        };
        if !self
            .device
            .features()
            .contains(Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        {
            return Ok(None);
        }
        let fd = dmabuf.fd.try_clone().context("duplicate DMA-BUF fd")?;
        let descriptor = TextureDescriptor {
            label: Some("focaldesk-dmabuf-texture"),
            size: Extent3d {
                width: surface.width,
                height: surface.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: texture_format(surface.format),
            usage: TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let hal_descriptor = hal::TextureDescriptor {
            label: descriptor.label,
            size: descriptor.size,
            mip_level_count: descriptor.mip_level_count,
            sample_count: descriptor.sample_count,
            dimension: descriptor.dimension,
            format: descriptor.format,
            usage: TextureUses::RESOURCE,
            memory_flags: hal::MemoryFlags::empty(),
            view_formats: descriptor.view_formats.to_vec(),
        };
        // SAFETY: the fd is an owned duplicate of the single-plane DMA-BUF
        // described by `surface`. The dimensions, format, modifier, stride,
        // and offset were validated by the Wayland DMA-BUF protocol handler.
        // Both HAL calls use this device and ownership of the duplicate passes
        // to Vulkan on successful import.
        let texture = unsafe {
            let hal_device = self
                .device
                .as_hal::<Vulkan>()
                .context("wgpu Vulkan HAL device is unavailable")?;
            let hal_texture = hal_device.texture_from_dmabuf_fd(
                fd,
                &hal_descriptor,
                dmabuf.modifier,
                u64::from(surface.stride),
                dmabuf.offset,
            )?;
            self.device.create_texture_from_hal::<Vulkan>(
                hal_texture,
                &descriptor,
                TextureUses::RESOURCE,
            )
        };
        Ok(Some(texture))
    }
}

impl PresentRenderer for WgpuVulkanRenderer {
    fn info(&self) -> &RendererInfo {
        &self.info
    }

    fn resize(&mut self, width: u32, height: u32) {
        let Some((width, height)) = surface_extent(width, height) else {
            return;
        };
        self.config.width = width;
        self.config.height = height;
        self.configure_surface();
    }

    fn present_frame(&mut self, surfaces: &[TextureQuad]) -> Result<PresentResult> {
        let (frame, reconfigure_after_present) = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => (frame, false),
            CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                return Ok(PresentResult::Skipped);
            }
            CurrentSurfaceTexture::Outdated => {
                self.configure_surface();
                return Ok(PresentResult::SurfaceReconfigured);
            }
            CurrentSurfaceTexture::Lost => {
                self.recreate_surface()?;
                return Ok(PresentResult::SurfaceReconfigured);
            }
            CurrentSurfaceTexture::Validation => {
                anyhow::bail!("wgpu rejected acquisition of the nested Vulkan surface");
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        let mut surface_vertices = Vec::with_capacity(surfaces.len() * 6);
        self.frame_no = self.frame_no.wrapping_add(1);
        let mut surface_cache_keys = Vec::with_capacity(surfaces.len());
        let mut surface_ranges = Vec::with_capacity(surfaces.len());
        let max_texture_dimension = self.device.limits().max_texture_dimension_2d;
        for surface in surfaces {
            let Some(row_bytes) = surface.width.checked_mul(4) else {
                continue;
            };
            if surface.width == 0
                || surface.height == 0
                || surface.width > max_texture_dimension
                || surface.height > max_texture_dimension
                || surface.stride < row_bytes
                || surface.destination[2] <= 0
                || surface.destination[3] <= 0
                || !surface
                    .source_uv
                    .iter()
                    .all(|coordinate| coordinate.is_finite())
            {
                continue;
            }
            let Some(required_len) = (surface.stride as usize)
                .checked_mul((surface.height - 1) as usize)
                .and_then(|prefix| prefix.checked_add(row_bytes as usize))
            else {
                continue;
            };
            if surface.pixels.len() < required_len {
                tracing::warn!(
                    width = surface.width,
                    height = surface.height,
                    stride = surface.stride,
                    actual = surface.pixels.len(),
                    required = required_len,
                    "skipping truncated Wayland SHM surface"
                );
                continue;
            }

            let recreate = self
                .texture_cache
                .get(&surface.cache_key)
                .is_none_or(|cached| {
                    cached.width != surface.width
                        || cached.height != surface.height
                        || cached.format != surface.format
                });
            if recreate {
                let upload_descriptor = TextureDescriptor {
                    label: Some("focaldesk-compositor-texture"),
                    size: Extent3d {
                        width: surface.width,
                        height: surface.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: texture_format(surface.format),
                    usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                    view_formats: &[],
                };
                #[cfg(unix)]
                let (texture, external) = match self.import_dmabuf_texture(surface) {
                    Ok(Some(texture)) => {
                        self.dmabuf_imports = self.dmabuf_imports.saturating_add(1);
                        (texture, true)
                    }
                    Ok(None) => (self.device.create_texture(&upload_descriptor), false),
                    Err(error) => {
                        self.dmabuf_fallbacks = self.dmabuf_fallbacks.saturating_add(1);
                        tracing::warn!(%error, "Vulkan DMA-BUF import failed; using upload fallback");
                        (self.device.create_texture(&upload_descriptor), false)
                    }
                };
                #[cfg(not(unix))]
                let (texture, external) = (self.device.create_texture(&upload_descriptor), false);
                let texture_view = texture.create_view(&TextureViewDescriptor::default());
                let bind_group = self.device.create_bind_group(&BindGroupDescriptor {
                    label: Some("focaldesk-compositor-texture-bind-group"),
                    layout: &self.surface_bind_group_layout,
                    entries: &[
                        BindGroupEntry {
                            binding: 0,
                            resource: BindingResource::TextureView(&texture_view),
                        },
                        BindGroupEntry {
                            binding: 1,
                            resource: BindingResource::Sampler(&self.surface_sampler),
                        },
                    ],
                });
                self.texture_cache.insert(
                    surface.cache_key,
                    CachedTexture {
                        _texture: texture,
                        bind_group,
                        width: surface.width,
                        height: surface.height,
                        format: surface.format,
                        external,
                        last_used_frame: self.frame_no,
                    },
                );
            }
            let cached = self
                .texture_cache
                .get_mut(&surface.cache_key)
                .expect("cache entry was just validated");
            cached.last_used_frame = self.frame_no;
            let full_damage = [[0, 0, surface.width, surface.height]];
            let damage = if cached.external {
                [].as_slice()
            } else if recreate {
                full_damage.as_slice()
            } else {
                surface.damage.as_slice()
            };
            for &[x, y, width, height] in damage {
                let width = width.min(surface.width.saturating_sub(x));
                let height = height.min(surface.height.saturating_sub(y));
                if width == 0 || height == 0 {
                    continue;
                }
                let offset = u64::from(y) * u64::from(surface.stride) + u64::from(x) * 4;
                self.queue.write_texture(
                    TexelCopyTextureInfo {
                        texture: &cached._texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: TextureAspect::All,
                    },
                    &surface.pixels,
                    TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(surface.stride),
                        rows_per_image: Some(height),
                    },
                    Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }

            let [x, y, width, height] = surface.destination;
            let [uv_tl, uv_tr, uv_br, uv_bl] = transformed_uv(surface.source_uv, surface.transform);
            let left = x as f32 / self.config.width as f32 * 2.0 - 1.0;
            let right = (x as f32 + width as f32) / self.config.width as f32 * 2.0 - 1.0;
            let top = 1.0 - y as f32 / self.config.height as f32 * 2.0;
            let bottom = 1.0 - (y as f32 + height as f32) / self.config.height as f32 * 2.0;
            let start = surface_vertices.len() as u32;
            surface_vertices.extend_from_slice(&[
                SurfaceVertex {
                    position: [left, top],
                    uv: uv_tl,
                },
                SurfaceVertex {
                    position: [left, bottom],
                    uv: uv_bl,
                },
                SurfaceVertex {
                    position: [right, bottom],
                    uv: uv_br,
                },
                SurfaceVertex {
                    position: [left, top],
                    uv: uv_tl,
                },
                SurfaceVertex {
                    position: [right, bottom],
                    uv: uv_br,
                },
                SurfaceVertex {
                    position: [right, top],
                    uv: uv_tr,
                },
            ]);
            surface_ranges.push(start..surface_vertices.len() as u32);
            surface_cache_keys.push(surface.cache_key);
        }
        let active_keys: HashSet<_> = surface_cache_keys.iter().copied().collect();
        self.texture_cache.retain(|key, cached| {
            active_keys.contains(key) || self.frame_no.wrapping_sub(cached.last_used_frame) <= 120
        });
        let surface_vertex_buffer = (!surface_vertices.is_empty()).then(|| {
            self.device.create_buffer_init(&BufferInitDescriptor {
                label: Some("focaldesk-wayland-shm-vertices"),
                contents: bytemuck::cast_slice(&surface_vertices),
                usage: BufferUsages::VERTEX,
            })
        });

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("focaldesk-wgpu-probe-frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("focaldesk-wgpu-probe-clear"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color {
                            r: 0.015,
                            g: 0.025,
                            b: 0.055,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.probe_pipeline);
            pass.draw(0..3, 0..1);
            if let Some(vertex_buffer) = surface_vertex_buffer.as_ref() {
                pass.set_pipeline(&self.surface_pipeline);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                for (cache_key, vertices) in surface_cache_keys.iter().zip(surface_ranges.iter()) {
                    let bind_group = &self.texture_cache[cache_key].bind_group;
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.draw(vertices.clone(), 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        if reconfigure_after_present {
            self.configure_surface();
            Ok(PresentResult::SurfaceReconfigured)
        } else {
            Ok(PresentResult::Presented)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sized_surfaces_are_suspended_instead_of_configured() {
        assert_eq!(surface_extent(1920, 1080), Some((1920, 1080)));
        assert_eq!(surface_extent(0, 1080), None);
        assert_eq!(surface_extent(1920, 0), None);
    }

    #[test]
    fn wayland_transforms_map_texture_corners() {
        let source = [0.1, 0.2, 0.8, 0.9];
        let tl = [0.1, 0.2];
        let tr = [0.8, 0.2];
        let br = [0.8, 0.9];
        let bl = [0.1, 0.9];
        assert_eq!(
            transformed_uv(source, FrameTransform::Normal),
            [tl, tr, br, bl]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Rotate90),
            [bl, tl, tr, br]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Rotate180),
            [br, bl, tl, tr]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Rotate270),
            [tr, br, bl, tl]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Flipped),
            [tr, tl, bl, br]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Flipped90),
            [tl, bl, br, tr]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Flipped180),
            [bl, br, tr, tl]
        );
        assert_eq!(
            transformed_uv(source, FrameTransform::Flipped270),
            [br, tr, tl, bl]
        );
    }
}

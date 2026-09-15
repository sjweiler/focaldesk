//! Raw Vulkan renderer for compositor-owned DRM/GBM scanout buffers.
//!
//! This module deliberately has no Vulkan WSI surface or swapchain. KMS owns
//! presentation; Vulkan only imports DMA-BUF images, records rendering, and
//! exports a sync-file for the atomic commit's `IN_FENCE_FD`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::Arc;
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
const XRGB2101010: u32 = u32::from_le_bytes(*b"XR30");
const ARGB2101010: u32 = u32::from_le_bytes(*b"AR30");
const XBGR2101010: u32 = u32::from_le_bytes(*b"XB30");
const ABGR2101010: u32 = u32::from_le_bytes(*b"AB30");
const GPU_SUBMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const DAMAGE_HISTORY_LIMIT: usize = 64;
const DMA_BUF_SYNC_READ: u32 = 1;
type DamageRect = [i32; 4];
type DamageHistory = VecDeque<(u64, Vec<DamageRect>)>;

#[repr(C)]
struct DmaBufExportSyncFile {
    flags: u32,
    fd: i32,
}

nix::ioctl_readwrite!(dma_buf_export_sync_file, b'b', 2, DmaBufExportSyncFile);

#[repr(C)]
struct DmaBufImportSyncFile {
    flags: u32,
    fd: i32,
}

nix::ioctl_write_ptr!(dma_buf_import_sync_file, b'b', 3, DmaBufImportSyncFile);

const SOLID_SHADER: &str = r#"
struct Push {
    rect: vec4<f32>, color: vec4<f32>, geometry: vec4<f32>,
    matrix0: vec4<f32>, matrix1: vec4<f32>, matrix2: vec4<f32>,
}
var<immediate> pc: Push;

fn srgb_decode(value: f32) -> f32 {
    return select(value / 12.92, pow((value + 0.055) / 1.055, 2.4), value > 0.04045);
}

fn decode_color(color: vec4<f32>) -> vec4<f32> {
    if color.a <= 0.0 { return vec4(0.0); }
    let straight = color.rgb / color.a;
    let linear = vec3(srgb_decode(straight.r), srgb_decode(straight.g), srgb_decode(straight.b));
    let mapped = vec3(dot(pc.matrix0.xyz, linear), dot(pc.matrix1.xyz, linear), dot(pc.matrix2.xyz, linear));
    return vec4(mapped * color.a, color.a);
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
}

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0));
    let p = corners[i];
    var out: Out;
    out.position = vec4(pc.rect.xy + p * pc.rect.zw, 0.0, 1.0);
    out.color = pc.color;
    out.local = p * pc.geometry.xy;
    return out;
}

@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let radius = min(pc.geometry.z, min(pc.geometry.x, pc.geometry.y) * 0.5);
    if radius <= 0.0 {
        return decode_color(in.color);
    }
    let half_size = pc.geometry.xy * 0.5;
    let q = abs(in.local - half_size) - (half_size - vec2(radius));
    let distance = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    let coverage = 1.0 - smoothstep(-0.75, 0.75, distance);
    return decode_color(in.color) * coverage;
}
"#;

const TEXTURE_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Push {
    rect: vec4<f32>,
    uv0: vec4<f32>,
    uv1: vec4<f32>,
    tint: vec4<f32>,
    matrix0: vec4<f32>, matrix1: vec4<f32>, matrix2: vec4<f32>,
    params: vec4<f32>,
}
var<immediate> pc: Push;

fn srgb_decode(value: f32) -> f32 {
    return select(value / 12.92, pow((value + 0.055) / 1.055, 2.4), value > 0.04045);
}

fn decode_channel(value: f32) -> f32 {
    let mode = pc.params.x;
    if mode < 0.5 { return srgb_decode(value); }
    if mode < 1.5 { return value * pc.params.z; }
    if mode < 2.5 { return pow(max(value, 0.0), 2.2); }
    if mode < 3.5 {
        let m1 = 2610.0 / 16384.0;
        let m2 = 2523.0 / 32.0;
        let c1 = 3424.0 / 4096.0;
        let c2 = 2413.0 / 128.0;
        let c3 = 2392.0 / 128.0;
        let p = pow(clamp(value, 0.0, 1.0), 1.0 / m2);
        let normalized = pow(max(p - c1, 0.0) / max(c2 - c3 * p, 0.000001), 1.0 / m1);
        return normalized * 10000.0 / max(pc.params.y, 1.0);
    }
    if mode < 4.5 { return srgb_decode(value); }
    if mode < 5.5 { return pow(max(value, 0.0), 2.4); }
    let a = 0.17883277;
    let b = 0.28466892;
    let c0 = 0.55991073;
    let low = value * value / 3.0;
    let high = (exp((value - c0) / a) + b) / 12.0;
    let scene = select(low, high, value >= 0.5);
    let reference = (exp((0.75 - c0) / a) + b) / 12.0;
    return pow(max(scene, 0.0), 1.2) / pow(reference, 1.2);
}

fn dither_code_value(color: vec3<f32>, position: vec2<f32>) -> vec3<f32> {
    if pc.params.w <= 1.0 { return color; }
    let code_step = 1.0 / (pow(2.0, pc.params.w) - 1.0);
    let pixel = floor(position);
    let a = fract(52.9829189 * fract(dot(pixel, vec2(0.06711056, 0.00583715))));
    let b = fract(52.9829189 * fract(dot(pixel, vec2(0.00583715, 0.06711056)) + 0.38196601));
    return clamp(color + vec3((a - b) * code_step), vec3(0.0), vec3(1.0));
}

fn decode_modulated_color(sampled: vec4<f32>, tint: vec4<f32>, position: vec2<f32>) -> vec4<f32> {
    let alpha = sampled.a * tint.a;
    if alpha <= 0.0 { return vec4(0.0); }
    var sampled_straight = sampled.rgb / sampled.a;
    let mode = pc.params.x;
    let extended = (mode >= 0.5 && mode < 1.5) || (mode >= 3.5 && mode < 4.5);
    if !extended { sampled_straight = clamp(sampled_straight, vec3(0.0), vec3(1.0)); }
    sampled_straight = dither_code_value(sampled_straight, position);
    let tint_straight = tint.rgb / tint.a;
    let sampled_linear = vec3(decode_channel(sampled_straight.r), decode_channel(sampled_straight.g), decode_channel(sampled_straight.b));
    let tint_linear = vec3(srgb_decode(tint_straight.r), srgb_decode(tint_straight.g), srgb_decode(tint_straight.b));
    let linear = sampled_linear * tint_linear;
    let mapped = vec3(dot(pc.matrix0.xyz, linear), dot(pc.matrix1.xyz, linear), dot(pc.matrix2.xyz, linear));
    return vec4(mapped * alpha, alpha);
}

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
    return decode_modulated_color(textureSample(image, image_sampler, in.uv), in.tint, in.position.xy);
}
"#;

const MESH_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Push {
    screen: vec4<f32>,
    matrix0: vec4<f32>, matrix1: vec4<f32>, matrix2: vec4<f32>,
}
var<immediate> pc: Push;

fn srgb_decode(value: f32) -> f32 {
    return select(value / 12.92, pow((value + 0.055) / 1.055, 2.4), value > 0.04045);
}

fn decode_modulated_color(sampled: vec4<f32>, tint: vec4<f32>) -> vec4<f32> {
    // egui's font atlas and vertex colors are both premultiplied sRGBA. Its
    // reference painters multiply them in gamma space first; doing two
    // independent unpremultiply/decode operations loses the font coverage
    // contract (and mishandles additive Color32 values).
    let encoded = sampled * tint;
    let alpha = encoded.a;
    if alpha <= 0.0 { return vec4(0.0); }
    let straight = clamp(encoded.rgb / alpha, vec3(0.0), vec3(1.0));
    let linear = vec3(srgb_decode(straight.r), srgb_decode(straight.g), srgb_decode(straight.b));
    let mapped = vec3(dot(pc.matrix0.xyz, linear), dot(pc.matrix1.xyz, linear), dot(pc.matrix2.xyz, linear));
    return vec4(mapped * alpha, alpha);
}

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
    return decode_modulated_color(textureSample(image, image_sampler, in.uv), in.color);
}
"#;

const OUTPUT_SHADER: &str = r#"
@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var lut: texture_2d<f32>;

struct Push {
    params0: vec4<f32>,
    params1: vec4<f32>,
    params2: vec4<f32>,
    matrix0: vec4<f32>,
    matrix1: vec4<f32>,
    matrix2: vec4<f32>,
    luma: vec4<f32>,
}
var<immediate> pc: Push;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let positions = array<vec2<f32>, 3>(
        vec2(-1.0, -1.0), vec2(-1.0, 3.0), vec2(3.0, -1.0));
    let uvs = array<vec2<f32>, 3>(
        vec2(0.0, 0.0), vec2(0.0, 2.0), vec2(2.0, 0.0));
    var out: Out;
    out.position = vec4(positions[i], 0.0, 1.0);
    out.uv = uvs[i];
    return out;
}

fn srgb_encode(value: f32) -> f32 {
    return select(value * 12.92, 1.055 * pow(max(value, 0.0), 1.0 / 2.4) - 0.055, value > 0.0031308);
}

fn gamma22_encode(value: f32) -> f32 {
    return pow(max(value, 0.0), 1.0 / 2.2);
}

fn encode_color(color: vec3<f32>) -> vec3<f32> {
    if pc.params0.y == 1.0 || pc.params0.y > 3.5 {
        return vec3(gamma22_encode(color.r), gamma22_encode(color.g), gamma22_encode(color.b));
    }
    return vec3(srgb_encode(color.r), srgb_encode(color.g), srgb_encode(color.b));
}

fn srgb_decode(value: f32) -> f32 {
    return select(value / 12.92, pow((value + 0.055) / 1.055, 2.4), value > 0.04045);
}

fn lut_sample_at(cell: vec3<f32>) -> vec3<f32> {
    let n = pc.params0.x;
    let bounded = clamp(cell, vec3(0.0), vec3(n - 1.0));
    let coordinate = vec2<i32>(i32(bounded.y * n + bounded.x), i32(bounded.z));
    return textureLoad(lut, coordinate, 0).rgb;
}

fn lut_lookup(color: vec3<f32>) -> vec3<f32> {
    let n = pc.params0.x;
    let position = clamp(color, vec3(0.0), vec3(1.0)) * (n - 1.0);
    let low = clamp(floor(position + vec3(0.00001)), vec3(0.0), vec3(n - 1.0));
    let fraction = clamp(position - low, vec3(0.0), vec3(1.0));
    let high = min(low + vec3(1.0), vec3(n - 1.0));
    let c000 = lut_sample_at(vec3(low.x, low.y, low.z));
    let c100 = lut_sample_at(vec3(high.x, low.y, low.z));
    let c010 = lut_sample_at(vec3(low.x, high.y, low.z));
    let c110 = lut_sample_at(vec3(high.x, high.y, low.z));
    let c001 = lut_sample_at(vec3(low.x, low.y, high.z));
    let c101 = lut_sample_at(vec3(high.x, low.y, high.z));
    let c011 = lut_sample_at(vec3(low.x, high.y, high.z));
    let c111 = lut_sample_at(vec3(high.x, high.y, high.z));
    let c00 = mix(c000, c100, fraction.x);
    let c10 = mix(c010, c110, fraction.x);
    let c01 = mix(c001, c101, fraction.x);
    let c11 = mix(c011, c111, fraction.x);
    return mix(mix(c00, c10, fraction.y), mix(c01, c11, fraction.y), fraction.z);
}

fn pq_oetf(nits: f32) -> f32 {
    let l = max(nits, 0.0) / 10000.0;
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let lm = pow(l, m1);
    return pow((c1 + c2 * lm) / (1.0 + c3 * lm), m2);
}

fn tone_map_nits(value: f32, source_peak: f32, display_peak: f32, white: f32) -> f32 {
    let knee = max(white, display_peak * 0.8);
    if value <= knee || display_peak <= knee || source_peak <= display_peak {
        return min(value, display_peak);
    }
    let peak = max(source_peak, knee + 0.0001);
    let range = max(display_peak - knee, 0.0001);
    let denominator = 1.0 - exp(-(peak - knee) / range);
    let numerator = 1.0 - exp(-(value - knee) / range);
    return min(knee + range * numerator / max(denominator, 0.0001), display_peak);
}

fn pq_dither(pixel: vec2<f32>) -> f32 {
    let p = floor(pixel);
    let a = fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))));
    let b = fract(52.9829189 * fract(dot(p, vec2(0.00583715, 0.06711056)) + 0.38196601));
    return (a - b) / 1023.0;
}

fn calibration_nits(uv: vec2<f32>, pattern: f32) -> vec3<f32> {
    if pattern < 1.5 {
        if uv.y < 0.5 {
            if uv.x < 0.25 { return vec3(100.0); }
            if uv.x < 0.5 { return vec3(203.0); }
            if uv.x < 0.75 { return vec3(300.0); }
            return vec3(pc.params0.z);
        }
        if uv.x < 0.25 { return vec3(200.0 / 0.2627, 0.0, 0.0); }
        if uv.x < 0.5 { return vec3(0.0, 200.0 / 0.6780, 0.0); }
        if uv.x < 0.75 { return vec3(0.0, 0.0, 200.0 / 0.0593); }
        return vec3(clamp((uv.x - 0.75) * 4.0, 0.0, 1.0) * pc.params0.z);
    }
    if pattern < 2.5 {
        let black = max(pc.params1.x, 0.001);
        if uv.x < 0.25 { return vec3(0.0); }
        if uv.x < 0.5 { return vec3(black); }
        if uv.x < 0.75 { return vec3(black * 2.0); }
        return vec3(black * 4.0);
    }
    if pattern < 3.5 { return vec3(pc.params1.y); }
    if pattern < 4.5 {
        let d = abs(uv - vec2(0.5));
        return select(vec3(0.0), vec3(pc.params0.z), d.x < 0.158114 && d.y < 0.158114);
    }
    return vec3(pc.params0.w);
}

fn encode_pq(scene_linear: vec3<f32>, pixel: vec2<f32>, uv: vec2<f32>) -> vec3<f32> {
    let calibration = pc.params2.y > 0.5;
    var nits: vec3<f32>;
    if calibration {
        nits = calibration_nits(uv, pc.params2.y);
    } else {
        var bt2020 = max(vec3(
            dot(pc.matrix0.xyz, scene_linear),
            dot(pc.matrix1.xyz, scene_linear),
            dot(pc.matrix2.xyz, scene_linear)), vec3(0.0));
        let scene_y = max(dot(bt2020, pc.luma.xyz), 0.0);
        bt2020 = max(vec3(scene_y) + (bt2020 - vec3(scene_y)) * pc.params1.w, vec3(0.0));
        nits = bt2020 * pc.params1.y;
    }
    var y = max(dot(nits, pc.luma.xyz), 0.0);
    if y > 0.0001 {
        if !calibration && y < pc.params1.y {
            let normalized = clamp(y / pc.params1.y, 0.0, 1.0);
            let shaped = pow(normalized, pc.params2.x) * pc.params1.y;
            nits *= shaped / y;
            y = shaped;
        }
        let mapped = tone_map_nits(
            y, max(pc.params1.z, pc.params0.z), pc.params0.z, pc.params1.y);
        nits *= mapped / y;
    }
    nits = min(nits, vec3(10000.0));
    var pq = vec3(pq_oetf(nits.r), pq_oetf(nits.g), pq_oetf(nits.b));
    if !calibration { pq += vec3(pq_dither(pixel)); }
    return clamp(pq, vec3(0.0), vec3(1.0));
}

@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let sampled = textureSample(scene, scene_sampler, in.uv);
    if sampled.a <= 0.0 { return vec4(0.0); }
    let straight = sampled.rgb / sampled.a;
    if pc.params0.y > 1.5 {
        return vec4(encode_pq(straight, in.position.xy, in.uv) * sampled.a, sampled.a);
    }
    let encoded = encode_color(straight);
    let corrected = lut_lookup(encoded);
    if pc.params0.y > 2.5 {
        return vec4(corrected * sampled.a, sampled.a);
    }
    let linear = vec3(srgb_decode(corrected.r), srgb_decode(corrected.g), srgb_decode(corrected.b));
    return vec4(linear * sampled.a, sampled.a);
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

struct LinearScene {
    resource: TextureResource,
    width: u32,
    height: u32,
    initialized: bool,
}

struct OutputResources {
    lut: ImageResource,
    descriptor_set: vk::DescriptorSet,
    scene_image: vk::Image,
    fingerprint: u64,
    grid_size: u32,
}

struct PendingFrame {
    submitted_at: Instant,
    fence: vk::Fence,
    command: vk::CommandBuffer,
    semaphore: vk::Semaphore,
    wait_semaphores: Vec<vk::Semaphore>,
    framebuffers: Vec<vk::Framebuffer>,
    retired_textures: Vec<TextureResource>,
    retired_outputs: Vec<OutputResources>,
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
    scene_render_pass: vk::RenderPass,
    output_render_pass: vk::RenderPass,
    solid: vk::Pipeline,
    texture: vk::Pipeline,
    mesh: vk::Pipeline,
    output: vk::Pipeline,
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

/// Encoded output-space RGB cube used for a post-composition ICC correction.
#[derive(Clone, Copy, Debug)]
pub struct AshDrmOutputLut<'a> {
    pub grid_size: u32,
    pub rgb: &'a [u8],
}

/// Parameters for the final scene-linear to BT.2020/ST 2084 output pass.
#[derive(Clone, Copy, Debug)]
pub struct AshDrmHdrOutput {
    pub peak_nits: f32,
    pub full_frame_peak_nits: f32,
    pub black_level_nits: f32,
    pub reference_white_nits: f32,
    pub source_peak_nits: f32,
    pub saturation: f32,
    pub midtone_gamma: f32,
    pub calibration_pattern: f32,
    pub scene_to_bt2020: [[f32; 3]; 3],
    pub bt2020_luma: [f32; 3],
}

/// Electrical SDR encoding expected by the selected output profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AshDrmTransfer {
    #[default]
    Srgb,
    Gamma22,
    /// Explicit sRGB encoding for a non-sRGB UNORM attachment.
    SrgbUnorm,
    /// Explicit gamma 2.2 encoding for a non-sRGB UNORM attachment.
    Gamma22Unorm,
    /// SMPTE ST 2084 carried in a BT.2020 RGB KMS framebuffer.
    Pq,
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
            XRGB2101010 | ARGB2101010 | XBGR2101010 | ABGR2101010 => {
                // All supported ten-bit DRM formats are one little-endian
                // packed u32 per pixel. Normalize the RGB fields to eight bit
                // for the existing screenshot/SHM capture contract. Captures
                // are opaque regardless of the scanout format's alpha bits.
                for pixel in self.pixels.chunks_exact_mut(4) {
                    let packed = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                    let (r, g, b) = if matches!(self.fourcc, XRGB2101010 | ARGB2101010) {
                        (
                            (packed >> 20) & 0x3ff,
                            (packed >> 10) & 0x3ff,
                            packed & 0x3ff,
                        )
                    } else {
                        (
                            packed & 0x3ff,
                            (packed >> 10) & 0x3ff,
                            (packed >> 20) & 0x3ff,
                        )
                    };
                    let to_u8 = |value: u32| ((value * 255 + 511) / 1023) as u8;
                    pixel.copy_from_slice(&[to_u8(r), to_u8(g), to_u8(b), 255]);
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
    output_descriptor_layout: vk::DescriptorSetLayout,
    solid_layout: vk::PipelineLayout,
    texture_layout: vk::PipelineLayout,
    output_layout: vk::PipelineLayout,
    sampler: vk::Sampler,
    pipelines: HashMap<vk::Format, Pipelines>,
    targets: HashMap<TargetKey, ImportedTarget>,
    linear_scenes: HashMap<u64, LinearScene>,
    output_resources: HashMap<u64, OutputResources>,
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
        let linear_features = unsafe {
            instance.get_physical_device_format_properties(
                physical_device,
                vk::Format::R16G16B16A16_SFLOAT,
            )
        }
        .optimal_tiling_features;
        ensure!(
            linear_features.contains(
                vk::FormatFeatureFlags::COLOR_ATTACHMENT
                    | vk::FormatFeatureFlags::SAMPLED_IMAGE
                    | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
            ),
            "Vulkan device cannot render and sample the FP16 compositor scene"
        );

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
        let output_descriptor_layout = unsafe {
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
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(2)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
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
            .size(96)];
        let texture_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(128)];
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
        let output_layouts = [output_descriptor_layout];
        let output_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .size(112)];
        let output_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&output_layouts)
                    .push_constant_ranges(&output_range),
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
            output_descriptor_layout,
            solid_layout,
            texture_layout,
            output_layout,
            sampler,
            pipelines: HashMap::new(),
            targets: HashMap::new(),
            linear_scenes: HashMap::new(),
            output_resources: HashMap::new(),
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
        // Client buffers remain UNORM and are decoded explicitly after
        // un-premultiplication in the fragment shaders. Rendering through an
        // SRGB view provides the paired linear-to-sRGB encode on attachment
        // writes while retaining the DRM buffer's established byte layout.
        for (fourcc, render_format, sample_format) in [
            (
                XRGB8888,
                vk::Format::B8G8R8A8_SRGB,
                vk::Format::B8G8R8A8_UNORM,
            ),
            (
                ARGB8888,
                vk::Format::B8G8R8A8_SRGB,
                vk::Format::B8G8R8A8_UNORM,
            ),
            (
                XBGR8888,
                vk::Format::R8G8B8A8_SRGB,
                vk::Format::R8G8B8A8_UNORM,
            ),
            (
                ABGR8888,
                vk::Format::R8G8B8A8_SRGB,
                vk::Format::R8G8B8A8_UNORM,
            ),
            (
                XRGB2101010,
                vk::Format::A2R10G10B10_UNORM_PACK32,
                vk::Format::A2R10G10B10_UNORM_PACK32,
            ),
            (
                ARGB2101010,
                vk::Format::A2R10G10B10_UNORM_PACK32,
                vk::Format::A2R10G10B10_UNORM_PACK32,
            ),
            (
                XBGR2101010,
                vk::Format::A2B10G10R10_UNORM_PACK32,
                vk::Format::A2B10G10R10_UNORM_PACK32,
            ),
            (
                ABGR2101010,
                vk::Format::A2B10G10R10_UNORM_PACK32,
                vk::Format::A2B10G10R10_UNORM_PACK32,
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
                    || modifier.drm_format_modifier_plane_count == 0
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
        output_matrix: [[f32; 3]; 3],
        output_transfer: AshDrmTransfer,
        output_lut: Option<AshDrmOutputLut<'_>>,
        hdr_output: Option<AshDrmHdrOutput>,
    ) -> Result<AshDrmSubmission> {
        self.poll()?;
        ensure!(
            target.width > 0 && target.height > 0,
            "zero-sized DRM render target"
        );
        let identity_lut;
        let output_lut = if let Some(lut) = output_lut {
            validate_output_lut(lut)?;
            lut
        } else {
            identity_lut = identity_output_lut();
            AshDrmOutputLut {
                grid_size: 2,
                rgb: &identity_lut,
            }
        };
        let lut_fingerprint = output_lut_fingerprint(output_lut);
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
        let linear_compatible = self
            .linear_scenes
            .get(&stream_id)
            .is_some_and(|scene| scene.width == target.width && scene.height == target.height);
        let retired_linear = if linear_compatible {
            None
        } else {
            let image = self.create_owned_image(
                target.width,
                target.height,
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            )?;
            let resource = self.texture_resource(image, false)?;
            self.linear_scenes
                .insert(
                    stream_id,
                    LinearScene {
                        resource,
                        width: target.width,
                        height: target.height,
                        initialized: false,
                    },
                )
                .map(|scene| scene.resource)
        };
        let linear_image = self.linear_scenes[&stream_id].resource.image.image;
        let linear_view = self.linear_scenes[&stream_id].resource.image.view;
        let linear_initialized = self.linear_scenes[&stream_id].initialized;

        self.frame_no = self.frame_no.wrapping_add(1);
        let stream_frame = self
            .stream_frames
            .entry(stream_id)
            .and_modify(|frame| *frame = frame.wrapping_add(1))
            .or_insert(1);
        let stream_frame = *stream_frame;
        let history = self.damage_history.entry(stream_id).or_default();
        let output_areas = effective_damage_regions(
            initialized,
            last_target_frame,
            history,
            damage,
            target.width,
            target.height,
        );
        let scene_areas = effective_damage_regions(
            linear_initialized,
            0,
            &DamageHistory::new(),
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
        if let Some(resource) = retired_linear {
            retired_textures.push(resource);
        }
        let mut staging = Vec::new();
        let retired_output = self.prepare_output_resources(
            command,
            stream_id,
            linear_image,
            linear_view,
            output_lut,
            lut_fingerprint,
            &mut staging,
        )?;
        let output_descriptor = self.output_resources[&stream_id].descriptor_set;
        let output_grid_size = self.output_resources[&stream_id].grid_size;
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
            // A DMA-BUF is shared memory: client damage changes its contents,
            // not its identity or import metadata. Keep the Vulkan image/view
            // and sample the updated allocation instead of importing and
            // retiring a new external image every frame.
            let external = surface.dmabuf.is_some();
            if texture_needs_refresh(compatible, external, !surface.damage.is_empty()) {
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

        let linear_to_attachment = vk::ImageMemoryBarrier::default()
            .src_access_mask(if linear_initialized {
                vk::AccessFlags::SHADER_READ
            } else {
                vk::AccessFlags::empty()
            })
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .old_layout(if linear_initialized {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .image(linear_image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                if linear_initialized {
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                } else {
                    vk::PipelineStageFlags::TOP_OF_PIPE
                },
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[linear_to_attachment],
            );
        }

        let scene_framebuffer = unsafe {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.pipelines[&format].scene_render_pass)
                    .attachments(&[linear_view])
                    .width(target.width)
                    .height(target.height)
                    .layers(1),
                None,
            )
        }?;
        for render_area in scene_areas {
            self.record_scene_pass(
                command,
                scene_framebuffer,
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
                output_matrix,
            );
        }
        let linear_to_sample = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image(linear_image)
            .subresource_range(color_range());
        unsafe {
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[linear_to_sample],
            );
        }
        let output_framebuffer = unsafe {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.pipelines[&format].output_render_pass)
                    .attachments(&[target_view])
                    .width(target.width)
                    .height(target.height)
                    .layers(1),
                None,
            )
        }?;
        for render_area in output_areas {
            self.record_output_pass(
                command,
                output_framebuffer,
                format,
                render_area,
                target.width,
                target.height,
                output_descriptor,
                output_grid_size,
                output_transfer,
                hdr_output,
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
        self.linear_scenes.get_mut(&stream_id).unwrap().initialized = true;

        // Vulkan external-memory imports do not implicitly wait for writers in
        // the DMA-BUF reservation object. Snapshot each distinct client
        // buffer's writer fences and import them as temporary binary
        // semaphores. The queue-family barriers above transfer ownership but
        // are not a substitute for producer synchronization.
        let mut implicit_dmabuf_ids = HashSet::new();
        let mut implicit_dmabufs = Vec::new();
        for (surface, texture) in surfaces.iter().zip(&textures) {
            let Some(dmabuf) = surface.dmabuf.as_ref() else {
                continue;
            };
            if !texture.is_some_and(|texture| texture.foreign) {
                continue;
            }
            let Some(plane) = dmabuf.planes.first() else {
                continue;
            };
            let identity = fd_identity(plane.as_raw_fd())?;
            if implicit_dmabuf_ids.insert(identity) {
                implicit_dmabufs.push(plane.clone());
            }
        }
        let mut wait_semaphores = Vec::with_capacity(implicit_dmabufs.len());
        for dmabuf in &implicit_dmabufs {
            let Some(sync_file) = export_dmabuf_read_fence(dmabuf.as_raw_fd())? else {
                continue;
            };
            match self.import_sync_file_semaphore(sync_file) {
                Ok(semaphore) => wait_semaphores.push(semaphore),
                Err(error) => {
                    unsafe {
                        for semaphore in wait_semaphores.drain(..) {
                            self.device.destroy_semaphore(semaphore, None);
                        }
                    }
                    return Err(error);
                }
            }
        }

        let mut export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
        let semaphore = match unsafe {
            self.device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut export),
                None,
            )
        } {
            Ok(semaphore) => semaphore,
            Err(error) => {
                unsafe {
                    for wait in wait_semaphores.drain(..) {
                        self.device.destroy_semaphore(wait, None);
                    }
                }
                return Err(error).context("create Vulkan KMS release semaphore");
            }
        };
        let fence = match unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        } {
            Ok(fence) => fence,
            Err(error) => {
                unsafe {
                    for wait in wait_semaphores.drain(..) {
                        self.device.destroy_semaphore(wait, None);
                    }
                    self.device.destroy_semaphore(semaphore, None);
                }
                return Err(error).context("create Vulkan completion fence");
            }
        };
        let signal = [semaphore];
        let commands = [command];
        let wait_stages = vec![vk::PipelineStageFlags::FRAGMENT_SHADER; wait_semaphores.len()];
        if let Err(error) = unsafe {
            self.device.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait_semaphores)
                    .wait_dst_stage_mask(&wait_stages)
                    .command_buffers(&commands)
                    .signal_semaphores(&signal)],
                fence,
            )
        } {
            unsafe {
                for wait in wait_semaphores.drain(..) {
                    self.device.destroy_semaphore(wait, None);
                }
                self.device.destroy_semaphore(semaphore, None);
                self.device.destroy_fence(fence, None);
            }
            return Err(error).context("submit raw Vulkan composition");
        }
        let raw_fd = unsafe {
            self.external_semaphore_fd.get_semaphore_fd(
                &vk::SemaphoreGetFdInfoKHR::default()
                    .semaphore(semaphore)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD),
            )
        }?;
        let fence_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        // Publish the Vulkan read completion back into the implicit-sync
        // reservation objects. Wayland buffer retention already prevents
        // normal reuse, while this also protects implicit consumers which use
        // the DMA-BUF reservation state directly.
        for dmabuf in &implicit_dmabufs {
            let result = fence_fd
                .try_clone()
                .context("duplicate Vulkan release fence")
                .and_then(|release| import_dmabuf_read_fence(dmabuf.as_raw_fd(), release));
            if let Err(error) = result {
                tracing::warn!(%error, "failed to publish Vulkan read fence to DMA-BUF");
            }
        }
        let retentions = surfaces
            .iter()
            .filter_map(|surface| surface.retention.clone())
            .collect();
        self.pending.push(PendingFrame {
            submitted_at: Instant::now(),
            fence,
            command,
            semaphore,
            wait_semaphores,
            framebuffers: vec![scene_framebuffer, output_framebuffer],
            retired_textures,
            retired_outputs: retired_output.into_iter().collect(),
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
        let scene_attachment = [vk::AttachmentDescription::default()
            .format(vk::Format::R16G16B16A16_SFLOAT)
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
        let scene_render_pass = unsafe {
            self.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&scene_attachment)
                    .subpasses(&subpass),
                None,
            )
        }?;
        let output_attachment = [vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let output_render_pass = unsafe {
            self.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&output_attachment)
                    .subpasses(&subpass),
                None,
            )
        }?;
        let solid = self.create_pipeline(
            scene_render_pass,
            self.solid_layout,
            SOLID_SHADER,
            false,
            false,
        )?;
        let texture = self.create_pipeline(
            scene_render_pass,
            self.texture_layout,
            TEXTURE_SHADER,
            true,
            false,
        )?;
        let mesh = self.create_pipeline(
            scene_render_pass,
            self.texture_layout,
            MESH_SHADER,
            true,
            true,
        )?;
        let output = self.create_pipeline(
            output_render_pass,
            self.output_layout,
            OUTPUT_SHADER,
            false,
            false,
        )?;
        self.pipelines.insert(
            format,
            Pipelines {
                scene_render_pass,
                output_render_pass,
                solid,
                texture,
                mesh,
                output,
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
        output_matrix: [[f32; 3]; 3],
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
                    .render_pass(self.pipelines[&format].scene_render_pass)
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
        self.draw_solids(command, format, background, width, height, output_matrix);
        for (index, (surface, texture)) in surfaces.iter().zip(textures).enumerate() {
            if index == overlay_after_surface {
                self.draw_solids(command, format, overlay, width, height, output_matrix);
            }
            if index == foreground_after_surface {
                self.draw_solids(command, format, foreground, width, height, output_matrix);
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
                    output_matrix,
                );
            }
            if let Some(texture) = texture {
                self.draw_texture(
                    command,
                    format,
                    surface,
                    texture,
                    width,
                    height,
                    output_matrix,
                );
            }
        }
        if overlay_after_surface >= surfaces.len() {
            self.draw_solids(command, format, overlay, width, height, output_matrix);
        }
        if foreground_after_surface >= surfaces.len() {
            self.draw_solids(command, format, foreground, width, height, output_matrix);
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
                output_matrix,
            );
        }
        unsafe { self.device.cmd_end_render_pass(command) };
    }

    #[allow(clippy::too_many_arguments)]
    fn record_output_pass(
        &self,
        command: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
        format: vk::Format,
        render_area: vk::Rect2D,
        width: u32,
        height: u32,
        scene_descriptor: vk::DescriptorSet,
        grid_size: u32,
        output_transfer: AshDrmTransfer,
        hdr_output: Option<AshDrmHdrOutput>,
    ) {
        unsafe {
            self.device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.pipelines[&format].output_render_pass)
                    .framebuffer(framebuffer)
                    .render_area(render_area),
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
            self.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines[&format].output,
            );
            self.device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.output_layout,
                0,
                &[scene_descriptor],
                &[],
            );
            let hdr = hdr_output.unwrap_or(AshDrmHdrOutput {
                peak_nits: 1.0,
                full_frame_peak_nits: 1.0,
                black_level_nits: 0.0,
                reference_white_nits: 1.0,
                source_peak_nits: 1.0,
                saturation: 1.0,
                midtone_gamma: 1.0,
                calibration_pattern: 0.0,
                scene_to_bt2020: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                bt2020_luma: [0.2627, 0.6780, 0.0593],
            });
            let push = [
                grid_size as f32,
                match output_transfer {
                    AshDrmTransfer::Srgb => 0.0,
                    AshDrmTransfer::Gamma22 => 1.0,
                    AshDrmTransfer::Pq => 2.0,
                    AshDrmTransfer::SrgbUnorm => 3.0,
                    AshDrmTransfer::Gamma22Unorm => 4.0,
                },
                hdr.peak_nits,
                hdr.full_frame_peak_nits,
                hdr.black_level_nits,
                hdr.reference_white_nits,
                hdr.source_peak_nits,
                hdr.saturation,
                hdr.midtone_gamma,
                hdr.calibration_pattern,
                0.0,
                0.0,
                hdr.scene_to_bt2020[0][0],
                hdr.scene_to_bt2020[0][1],
                hdr.scene_to_bt2020[0][2],
                0.0,
                hdr.scene_to_bt2020[1][0],
                hdr.scene_to_bt2020[1][1],
                hdr.scene_to_bt2020[1][2],
                0.0,
                hdr.scene_to_bt2020[2][0],
                hdr.scene_to_bt2020[2][1],
                hdr.scene_to_bt2020[2][2],
                0.0,
                hdr.bt2020_luma[0],
                hdr.bt2020_luma[1],
                hdr.bt2020_luma[2],
                0.0,
            ];
            self.device.cmd_push_constants(
                command,
                self.output_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                as_bytes(&push),
            );
            self.device.cmd_draw(command, 3, 1, 0, 0);
            self.device.cmd_end_render_pass(command);
        }
    }

    fn draw_solids(
        &self,
        command: vk::CommandBuffer,
        format: vk::Format,
        solids: &[SolidQuad],
        width: u32,
        height: u32,
        output_matrix: [[f32; 3]; 3],
    ) {
        unsafe {
            self.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines[&format].solid,
            )
        };
        for solid in solids {
            let push = solid_push_constants(solid, width, height, output_matrix);
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
        output_matrix: [[f32; 3]; 3],
    ) {
        let uvs = transformed_uv(surface.source_uv, surface.transform);
        let mut push = [0.0f32; 32];
        push[..4].copy_from_slice(&ndc_rect(surface.destination, width, height));
        push[4..8].copy_from_slice(&[uvs[0][0], uvs[0][1], uvs[1][0], uvs[1][1]]);
        push[8..12].copy_from_slice(&[uvs[2][0], uvs[2][1], uvs[3][0], uvs[3][1]]);
        push[12..16].copy_from_slice(&surface.tint);
        let matrix = multiply_3x3(output_matrix, surface.color_transform.client_to_scene);
        write_push_matrix(&mut push[16..28], matrix);
        push[28..].copy_from_slice(&[
            surface.color_transform.transfer as u32 as f32,
            surface.color_transform.reference_white_nits.max(1.0),
            surface.color_transform.linear_to_scene_scale,
            surface.color_transform.source_bits,
        ]);
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
        output_matrix: [[f32; 3]; 3],
    ) {
        let mut screen = [0.0_f32; 16];
        screen[..4].copy_from_slice(&mesh_clip_transform(width, height));
        write_push_matrix(&mut screen[4..], output_matrix);
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

    #[allow(clippy::too_many_arguments)]
    fn prepare_output_resources(
        &mut self,
        command: vk::CommandBuffer,
        stream_id: u64,
        scene_image: vk::Image,
        scene_view: vk::ImageView,
        lut: AshDrmOutputLut<'_>,
        fingerprint: u64,
        staging: &mut Vec<BufferResource>,
    ) -> Result<Option<OutputResources>> {
        let compatible = self
            .output_resources
            .get(&stream_id)
            .is_some_and(|resource| {
                resource.scene_image == scene_image && resource.fingerprint == fingerprint
            });
        if compatible {
            return Ok(None);
        }

        let atlas_width = lut
            .grid_size
            .checked_mul(lut.grid_size)
            .context("ICC LUT atlas width overflow")?;
        let mut rgba = Vec::with_capacity(lut.rgb.len() / 3 * 4);
        for rgb in lut.rgb.chunks_exact(3) {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        let image = self.create_owned_image(
            atlas_width,
            lut.grid_size,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        )?;
        let stage = self.create_staging(&rgba)?;
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
                    .buffer_row_length(atlas_width)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: atlas_width,
                        height: lut.grid_size,
                        depth: 1,
                    })],
            );
            let to_sample = vk::ImageMemoryBarrier::default()
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
                &[to_sample],
            );
        }
        staging.push(stage);

        let layouts = [self.output_descriptor_layout];
        let descriptor_set = unsafe {
            self.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&layouts),
            )
        }?[0];
        let scene_info = [vk::DescriptorImageInfo::default()
            .image_view(scene_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let sampler_info = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        let lut_info = [vk::DescriptorImageInfo::default()
            .image_view(image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        unsafe {
            self.device.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(&scene_info),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::SAMPLER)
                        .image_info(&sampler_info),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(2)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(&lut_info),
                ],
                &[],
            );
        }
        Ok(self.output_resources.insert(
            stream_id,
            OutputResources {
                lut: image,
                descriptor_set,
                scene_image,
                fingerprint,
                grid_size: lut.grid_size,
            },
        ))
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
                planes: dmabuf.planes.clone(),
                offsets: dmabuf.offsets.clone(),
                strides: dmabuf.strides.clone(),
                fourcc: dmabuf.fourcc,
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
            dmabuf_planes_share_object(&target.planes)?,
            "disjoint per-plane DMA-BUF memory is not implemented"
        );
        let layouts = target
            .offsets
            .iter()
            .zip(&target.strides)
            .map(|(&offset, &stride)| vk::SubresourceLayout {
                offset: u64::from(offset),
                row_pitch: u64::from(stride),
                ..Default::default()
            })
            .collect::<Vec<_>>();
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
        let components = texture_components(target.fourcc);
        let view = unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .components(components)
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

    fn import_sync_file_semaphore(&self, sync_file: OwnedFd) -> Result<vk::Semaphore> {
        let semaphore = unsafe {
            self.device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .context("create DMA-BUF acquire semaphore")?;
        let raw_fd = sync_file.into_raw_fd();
        let import = vk::ImportSemaphoreFdInfoKHR::default()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::TEMPORARY)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
            .fd(raw_fd);
        if let Err(error) = unsafe { self.external_semaphore_fd.import_semaphore_fd(&import) } {
            // Vulkan consumes a SYNC_FD only after a successful import.
            unsafe {
                drop(OwnedFd::from_raw_fd(raw_fd));
                self.device.destroy_semaphore(semaphore, None);
            }
            return Err(error).context("import DMA-BUF acquire sync file into Vulkan");
        }
        Ok(semaphore)
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
            for output in frame.retired_outputs {
                self.device
                    .free_descriptor_sets(self.descriptor_pool, &[output.descriptor_set])
                    .ok();
                self.destroy_image(output.lut);
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
            for framebuffer in frame.framebuffers {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for semaphore in frame.wait_semaphores {
                self.device.destroy_semaphore(semaphore, None);
            }
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
                for output in frame.retired_outputs {
                    self.device
                        .free_descriptor_sets(self.descriptor_pool, &[output.descriptor_set])
                        .ok();
                    self.device.destroy_image_view(output.lut.view, None);
                    self.device.destroy_image(output.lut.image, None);
                    self.device.free_memory(output.lut.memory, None);
                }
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
                for framebuffer in frame.framebuffers {
                    self.device.destroy_framebuffer(framebuffer, None);
                }
                for semaphore in frame.wait_semaphores {
                    self.device.destroy_semaphore(semaphore, None);
                }
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
            for (_, output) in self.output_resources.drain() {
                self.device
                    .free_descriptor_sets(self.descriptor_pool, &[output.descriptor_set])
                    .ok();
                self.device.destroy_image_view(output.lut.view, None);
                self.device.destroy_image(output.lut.image, None);
                self.device.free_memory(output.lut.memory, None);
            }
            for (_, scene) in self.linear_scenes.drain() {
                self.device
                    .free_descriptor_sets(self.descriptor_pool, &[scene.resource.descriptor_set])
                    .ok();
                self.device
                    .destroy_image_view(scene.resource.image.view, None);
                self.device.destroy_image(scene.resource.image.image, None);
                self.device.free_memory(scene.resource.image.memory, None);
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
                self.device.destroy_pipeline(pipeline.output, None);
                self.device
                    .destroy_render_pass(pipeline.scene_render_pass, None);
                self.device
                    .destroy_render_pass(pipeline.output_render_pass, None);
            }
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_pipeline_layout(self.solid_layout, None);
            self.device
                .destroy_pipeline_layout(self.texture_layout, None);
            self.device
                .destroy_pipeline_layout(self.output_layout, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.output_descriptor_layout, None);
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
        XRGB8888 | ARGB8888 => Ok(vk::Format::B8G8R8A8_SRGB),
        XBGR8888 | ABGR8888 => Ok(vk::Format::R8G8B8A8_SRGB),
        XRGB2101010 | ARGB2101010 => Ok(vk::Format::A2R10G10B10_UNORM_PACK32),
        XBGR2101010 | ABGR2101010 => Ok(vk::Format::A2B10G10R10_UNORM_PACK32),
        _ => bail!("unsupported DRM target fourcc 0x{fourcc:08x}"),
    }
}

fn texture_vk_format(format: FramePixelFormat) -> vk::Format {
    match format {
        // Preserve encoded SDR samples for explicit un-premultiply and decode
        // in the fragment shaders. Sampling through SRGB views would decode
        // premultiplied RGB before alpha is removed and produce dark fringes.
        FramePixelFormat::Bgra8Srgb => vk::Format::B8G8R8A8_UNORM,
        FramePixelFormat::Rgba8Srgb => vk::Format::R8G8B8A8_UNORM,
        FramePixelFormat::Bgra10Unorm => vk::Format::A2R10G10B10_UNORM_PACK32,
        FramePixelFormat::Rgba10Unorm => vk::Format::A2B10G10R10_UNORM_PACK32,
    }
}

fn texture_components(fourcc: u32) -> vk::ComponentMapping {
    vk::ComponentMapping::default().a(
        if matches!(fourcc, XRGB8888 | XBGR8888 | XRGB2101010 | XBGR2101010) {
            vk::ComponentSwizzle::ONE
        } else {
            vk::ComponentSwizzle::IDENTITY
        },
    )
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

#[cfg(unix)]
fn dmabuf_planes_share_object(planes: &[Arc<OwnedFd>]) -> Result<bool> {
    let Some(first) = planes.first() else {
        return Ok(false);
    };
    let identity = fd_identity(first.as_raw_fd())?;
    planes.iter().skip(1).try_fold(true, |same, plane| {
        Ok(same && fd_identity(plane.as_raw_fd())? == identity)
    })
}

#[cfg(unix)]
fn fd_identity(fd: RawFd) -> Result<(u64, u64)> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("stat DMA-BUF plane");
    }
    let value = unsafe { value.assume_init() };
    Ok((value.st_dev, value.st_ino))
}

#[cfg(unix)]
fn export_dmabuf_read_fence(fd: RawFd) -> Result<Option<OwnedFd>> {
    let mut export = DmaBufExportSyncFile {
        flags: DMA_BUF_SYNC_READ,
        fd: -1,
    };
    match unsafe { dma_buf_export_sync_file(fd, &mut export) } {
        Ok(_) => {
            ensure!(export.fd >= 0, "DMA-BUF fence export returned no sync file");
            Ok(Some(unsafe { OwnedFd::from_raw_fd(export.fd) }))
        }
        Err(nix::errno::Errno::ENOTTY | nix::errno::Errno::EINVAL | nix::errno::Errno::ENOSYS) => {
            // Old kernels or non-DMA-BUF test descriptors cannot provide a
            // reservation fence. Explicit-sync commit blockers and Wayland
            // buffer retention remain in force for those inputs.
            Ok(None)
        }
        Err(error) => Err(error).context("export DMA-BUF implicit read fence"),
    }
}

#[cfg(unix)]
fn import_dmabuf_read_fence(dmabuf_fd: RawFd, sync_fd: OwnedFd) -> Result<()> {
    let import = DmaBufImportSyncFile {
        flags: DMA_BUF_SYNC_READ,
        fd: sync_fd.as_raw_fd(),
    };
    match unsafe { dma_buf_import_sync_file(dmabuf_fd, &import) } {
        Ok(_) => Ok(()),
        Err(nix::errno::Errno::ENOTTY | nix::errno::Errno::EINVAL | nix::errno::Errno::ENOSYS) => {
            Ok(())
        }
        Err(error) => Err(error).context("import Vulkan read fence into DMA-BUF"),
    }
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
        y as f32 / height as f32 * 2.0 - 1.0,
        w as f32 / width as f32 * 2.0,
        h as f32 / height as f32 * 2.0,
    ]
}

fn mesh_clip_transform(width: u32, height: u32) -> [f32; 4] {
    [2.0 / width as f32, 2.0 / height as f32, -1.0, -1.0]
}

fn solid_push_constants(
    solid: &SolidQuad,
    width: u32,
    height: u32,
    output_matrix: [[f32; 3]; 3],
) -> [f32; 24] {
    let mut push = [0.0f32; 24];
    push[..4].copy_from_slice(&ndc_rect(solid.destination, width, height));
    push[4..8].copy_from_slice(&solid.color);
    push[8..12].copy_from_slice(&[
        solid.destination[2].max(0) as f32,
        solid.destination[3].max(0) as f32,
        solid.corner_radius.max(0.0),
        0.0,
    ]);
    write_push_matrix(&mut push[12..], output_matrix);
    push
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

fn validate_output_lut(lut: AshDrmOutputLut<'_>) -> Result<()> {
    ensure!(
        (2..=65).contains(&lut.grid_size),
        "ICC LUT grid must contain between 2 and 65 samples per axis"
    );
    let expected = usize::try_from(lut.grid_size)
        .ok()
        .and_then(|grid| grid.checked_pow(3))
        .and_then(|samples| samples.checked_mul(3))
        .context("ICC LUT dimensions overflow")?;
    ensure!(
        lut.rgb.len() == expected,
        "ICC LUT contains {} bytes; expected {expected}",
        lut.rgb.len()
    );
    Ok(())
}

fn output_lut_fingerprint(lut: AshDrmOutputLut<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    lut.grid_size.hash(&mut hasher);
    lut.rgb.hash(&mut hasher);
    hasher.finish()
}

fn identity_output_lut() -> Vec<u8> {
    let mut rgb = Vec::with_capacity(2 * 2 * 2 * 3);
    for blue in [0, 255] {
        for green in [0, 255] {
            for red in [0, 255] {
                rgb.extend_from_slice(&[red, green, blue]);
            }
        }
    }
    rgb
}

fn write_push_matrix(destination: &mut [f32], matrix: [[f32; 3]; 3]) {
    debug_assert!(destination.len() >= 12);
    for (row, values) in matrix.into_iter().enumerate() {
        let start = row * 4;
        destination[start..start + 3].copy_from_slice(&values);
        destination[start + 3] = 0.0;
    }
}

fn multiply_3x3(left: [[f32; 3]; 3], right: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = left[row][0] * right[0][column]
                + left[row][1] * right[1][column]
                + left[row][2] * right[2][column];
        }
    }
    result
}

fn texture_needs_refresh(compatible: bool, external: bool, damaged: bool) -> bool {
    !compatible || (!external && damaged)
}

#[cfg(test)]
mod tests {
    use super::{
        compile_shader, dmabuf_planes_share_object, effective_damage_regions,
        export_dmabuf_read_fence, identity_output_lut, import_dmabuf_read_fence,
        mesh_clip_transform, multiply_3x3, ndc_rect, solid_push_constants, texture_components,
        texture_needs_refresh, texture_vk_format, transformed_uv, validate_output_lut, vk_format,
        AshDrmCapture, AshDrmOutputLut, ARGB2101010, ARGB8888, MESH_SHADER, OUTPUT_SHADER,
        SOLID_SHADER, TEXTURE_SHADER, XBGR2101010, XRGB2101010, XRGB8888,
    };
    use crate::{FramePixelFormat, FrameTransform, SolidQuad};
    use std::collections::VecDeque;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    #[test]
    fn raw_vulkan_shaders_compile_to_spirv() {
        for source in [SOLID_SHADER, TEXTURE_SHADER, MESH_SHADER, OUTPUT_SHADER] {
            let vertex = compile_shader(source, naga::ShaderStage::Vertex, "vs_main").unwrap();
            let fragment = compile_shader(source, naga::ShaderStage::Fragment, "fs_main").unwrap();
            assert_eq!(vertex.first().copied(), Some(0x0723_0203));
            assert_eq!(fragment.first().copied(), Some(0x0723_0203));
        }
    }

    #[test]
    fn external_textures_are_reimported_only_when_their_identity_changes() {
        assert!(texture_needs_refresh(false, true, false));
        assert!(texture_needs_refresh(false, true, true));
        assert!(!texture_needs_refresh(true, true, true));
        assert!(texture_needs_refresh(true, false, true));
        assert!(!texture_needs_refresh(true, false, false));
    }

    #[test]
    fn auxiliary_planes_must_share_one_dma_buf_memory_object() {
        let (first, second) = UnixStream::pair().unwrap();
        let first: OwnedFd = first.into();
        let duplicate = first.try_clone().unwrap();
        assert!(dmabuf_planes_share_object(&[Arc::new(first), Arc::new(duplicate)]).unwrap());

        let second: OwnedFd = second.into();
        let (third, _) = UnixStream::pair().unwrap();
        let third: OwnedFd = third.into();
        assert!(!dmabuf_planes_share_object(&[Arc::new(second), Arc::new(third)]).unwrap());
    }

    #[test]
    fn non_dmabuf_descriptors_report_no_implicit_fence_support() {
        use std::os::fd::AsRawFd;

        let (descriptor, sync_file) = UnixStream::pair().unwrap();
        assert!(export_dmabuf_read_fence(descriptor.as_raw_fd())
            .unwrap()
            .is_none());
        import_dmabuf_read_fence(descriptor.as_raw_fd(), sync_file.into()).unwrap();
    }

    #[test]
    fn output_rect_is_converted_to_vulkan_clip_space() {
        assert_eq!(ndc_rect([0, 0, 100, 50], 100, 50), [-1.0, -1.0, 2.0, 2.0]);
        assert_eq!(ndc_rect([0, 40, 100, 10], 100, 50), [-1.0, 0.6, 2.0, 0.4]);
        assert_eq!(mesh_clip_transform(100, 50), [0.02, 0.04, -1.0, -1.0]);
    }

    #[test]
    fn solid_push_constants_pack_geometry_before_the_matrix() {
        let solid = SolidQuad {
            destination: [10, 20, 30, 40],
            color: [0.1, 0.2, 0.3, 0.4],
            corner_radius: 5.0,
        };
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let push = solid_push_constants(&solid, 100, 100, identity);
        assert_eq!(&push[4..8], &solid.color);
        assert_eq!(&push[8..12], &[30.0, 40.0, 5.0, 0.0]);
        assert_eq!(&push[12..16], &[1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn encoded_sdr_textures_pair_manual_decode_with_srgb_output_encode() {
        assert_eq!(
            texture_vk_format(FramePixelFormat::Bgra8Srgb),
            ash::vk::Format::B8G8R8A8_UNORM
        );
        assert_eq!(
            texture_vk_format(FramePixelFormat::Rgba8Srgb),
            ash::vk::Format::R8G8B8A8_UNORM
        );
        assert_eq!(
            texture_vk_format(FramePixelFormat::Bgra10Unorm),
            ash::vk::Format::A2R10G10B10_UNORM_PACK32
        );
        assert_eq!(
            texture_vk_format(FramePixelFormat::Rgba10Unorm),
            ash::vk::Format::A2B10G10R10_UNORM_PACK32
        );
        assert_eq!(vk_format(ARGB8888).unwrap(), ash::vk::Format::B8G8R8A8_SRGB);
        assert_eq!(
            vk_format(ARGB2101010).unwrap(),
            ash::vk::Format::A2R10G10B10_UNORM_PACK32
        );
    }

    #[test]
    fn xrgb_client_buffers_are_sampled_as_opaque() {
        assert_eq!(
            texture_components(XRGB8888).a,
            ash::vk::ComponentSwizzle::ONE
        );
        assert_eq!(
            texture_components(XRGB2101010).a,
            ash::vk::ComponentSwizzle::ONE
        );
        assert_eq!(
            texture_components(ARGB8888).a,
            ash::vk::ComponentSwizzle::IDENTITY
        );
    }

    #[test]
    fn pq_output_pass_tone_maps_and_dithers_for_ten_bit_scanout() {
        assert!(OUTPUT_SHADER.contains("fn pq_oetf"));
        assert!(OUTPUT_SHADER.contains("fn tone_map_nits"));
        assert!(OUTPUT_SHADER.contains("/ 1023.0"));
        assert!(OUTPUT_SHADER.contains("dot(pc.matrix0.xyz, scene_linear)"));
    }

    #[test]
    fn identity_output_lut_matches_rgb_cube_atlas_order() {
        assert_eq!(
            identity_output_lut(),
            vec![
                0, 0, 0, 255, 0, 0, 0, 255, 0, 255, 255, 0, 0, 0, 255, 255, 0, 255, 0, 255, 255,
                255, 255, 255,
            ]
        );
    }

    #[test]
    fn output_lut_rejects_incomplete_cube() {
        let error = validate_output_lut(AshDrmOutputLut {
            grid_size: 2,
            rgb: &[0; 21],
        })
        .unwrap_err();
        assert!(error.to_string().contains("expected 24"));
    }

    #[test]
    fn client_and_output_gamut_matrices_are_composed_in_draw_order() {
        let output = [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0]];
        let client = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(
            multiply_3x3(output, client),
            [[2.0, 4.0, 6.0], [12.0, 15.0, 18.0], [28.0, 32.0, 36.0]]
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
    fn ten_bit_scanout_capture_is_normalized_to_rgba8() {
        let packed = (1023_u32 << 20) | (512_u32 << 10);
        let capture = AshDrmCapture {
            id: 8,
            width: 1,
            height: 1,
            fourcc: XRGB2101010,
            pixels: packed.to_le_bytes().to_vec(),
        };
        assert_eq!(capture.into_rgba8().unwrap(), vec![255, 128, 0, 255]);

        let packed = 1023_u32 | (512_u32 << 10);
        let capture = AshDrmCapture {
            id: 9,
            width: 1,
            height: 1,
            fourcc: XBGR2101010,
            pixels: packed.to_le_bytes().to_vec(),
        };
        assert_eq!(capture.into_rgba8().unwrap(), vec![255, 128, 0, 255]);
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

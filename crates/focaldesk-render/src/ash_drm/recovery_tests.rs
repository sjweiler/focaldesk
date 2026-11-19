use super::*;
use ash::vk::Handle;
use std::{cell::RefCell, ffi::c_void};

struct Driver {
    events: Vec<String>,
    create: vk::Result,
    submit: vk::Result,
    wait: vk::Result,
    allocation_count: u64,
    fail_allocation: u64,
    view: vk::Result,
    map: vk::Result,
}
impl Default for Driver {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            create: vk::Result::SUCCESS,
            submit: vk::Result::SUCCESS,
            wait: vk::Result::SUCCESS,
            allocation_count: 0,
            fail_allocation: 0,
            view: vk::Result::SUCCESS,
            map: vk::Result::ERROR_MEMORY_MAP_FAILED,
        }
    }
}
thread_local! { static DRIVER: RefCell<Driver> = RefCell::new(Driver::default()); }
fn record(event: String) {
    DRIVER.with(|driver| driver.borrow_mut().events.push(event));
}
extern "system" fn create_fence(
    _: vk::Device,
    _: *const vk::FenceCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Fence,
) -> vk::Result {
    record("create fence".into());
    let result = DRIVER.with(|driver| driver.borrow().create);
    if result == vk::Result::SUCCESS {
        unsafe {
            *out = vk::Fence::from_raw(99);
        }
    }
    result
}
extern "system" fn submit(
    _: vk::Queue,
    count: u32,
    _: *const vk::SubmitInfo<'_>,
    _: vk::Fence,
) -> vk::Result {
    record(format!("submit {count}"));
    DRIVER.with(|driver| driver.borrow().submit)
}
extern "system" fn wait(
    _: vk::Device,
    _: u32,
    _: *const vk::Fence,
    _: vk::Bool32,
    timeout: u64,
) -> vk::Result {
    record(format!("wait {timeout}"));
    DRIVER.with(|driver| driver.borrow().wait)
}
macro_rules! destroy {
    ($name:ident, $handle:ty, $label:literal) => {
        extern "system" fn $name(
            _: vk::Device,
            handle: $handle,
            _: *const vk::AllocationCallbacks<'_>,
        ) {
            if handle.as_raw() != 0 {
                record(format!("{} {}", $label, handle.as_raw()));
            }
        }
    };
}
destroy!(destroy_fence, vk::Fence, "fence");
destroy!(destroy_semaphore, vk::Semaphore, "semaphore");
destroy!(destroy_buffer, vk::Buffer, "buffer");
destroy!(free_memory, vk::DeviceMemory, "memory");
destroy!(destroy_image, vk::Image, "image");
destroy!(destroy_view, vk::ImageView, "view");
destroy!(destroy_framebuffer, vk::Framebuffer, "framebuffer");
destroy!(destroy_pipeline, vk::Pipeline, "pipeline");
destroy!(destroy_pass, vk::RenderPass, "pass");
destroy!(destroy_shader, vk::ShaderModule, "shader");
extern "system" fn free_commands(
    _: vk::Device,
    _: vk::CommandPool,
    count: u32,
    commands: *const vk::CommandBuffer,
) {
    for command in unsafe { std::slice::from_raw_parts(commands, count as usize) } {
        record(format!("command {}", command.as_raw()));
    }
}
extern "system" fn free_descriptors(
    _: vk::Device,
    _: vk::DescriptorPool,
    count: u32,
    sets: *const vk::DescriptorSet,
) -> vk::Result {
    for set in unsafe { std::slice::from_raw_parts(sets, count as usize) } {
        record(format!("descriptor {}", set.as_raw()));
    }
    vk::Result::SUCCESS
}
destroy!(destroy_sampler, vk::Sampler, "sampler");
destroy!(
    destroy_pipeline_layout,
    vk::PipelineLayout,
    "pipeline layout"
);
destroy!(
    destroy_descriptor_pool,
    vk::DescriptorPool,
    "descriptor pool"
);
destroy!(
    destroy_descriptor_layout,
    vk::DescriptorSetLayout,
    "descriptor layout"
);
destroy!(destroy_command_pool, vk::CommandPool, "command pool");
extern "system" fn destroy_device(_: vk::Device, _: *const vk::AllocationCallbacks<'_>) {
    record("destroy device".into());
}
extern "system" fn destroy_instance(_: vk::Instance, _: *const vk::AllocationCallbacks<'_>) {
    record("destroy instance".into());
}
extern "system" fn get_device_proc(
    _: vk::Device,
    _: *const std::ffi::c_char,
) -> vk::PFN_vkVoidFunction {
    None
}
extern "system" fn create_image(
    _: vk::Device,
    _: *const vk::ImageCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Image,
) -> vk::Result {
    record("create image 50".into());
    unsafe {
        *out = vk::Image::from_raw(50);
    }
    vk::Result::SUCCESS
}
extern "system" fn image_requirements(
    _: vk::Device,
    _: vk::Image,
    out: *mut vk::MemoryRequirements,
) {
    unsafe {
        *out = vk::MemoryRequirements {
            size: 4096,
            alignment: 4,
            memory_type_bits: 1,
        };
    }
}
extern "system" fn allocate_memory(
    _: vk::Device,
    _: *const vk::MemoryAllocateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::DeviceMemory,
) -> vk::Result {
    DRIVER.with(|driver| {
        let mut driver = driver.borrow_mut();
        driver.allocation_count += 1;
        let index = driver.allocation_count;
        driver.events.push(format!("allocate {index}"));
        if index == driver.fail_allocation {
            return vk::Result::ERROR_OUT_OF_DEVICE_MEMORY;
        }
        unsafe {
            *out = vk::DeviceMemory::from_raw(200 + index);
        }
        vk::Result::SUCCESS
    })
}
extern "system" fn bind_image(
    _: vk::Device,
    _: vk::Image,
    _: vk::DeviceMemory,
    _: vk::DeviceSize,
) -> vk::Result {
    vk::Result::SUCCESS
}
extern "system" fn create_view(
    _: vk::Device,
    _: *const vk::ImageViewCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::ImageView,
) -> vk::Result {
    let result = DRIVER.with(|driver| driver.borrow().view);
    if result == vk::Result::SUCCESS {
        unsafe {
            *out = vk::ImageView::from_raw(51);
        }
    }
    result
}
extern "system" fn create_buffer(
    _: vk::Device,
    _: *const vk::BufferCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Buffer,
) -> vk::Result {
    record("create buffer 60".into());
    unsafe {
        *out = vk::Buffer::from_raw(60);
    }
    vk::Result::SUCCESS
}
extern "system" fn buffer_requirements(
    _: vk::Device,
    _: vk::Buffer,
    out: *mut vk::MemoryRequirements,
) {
    unsafe {
        *out = vk::MemoryRequirements {
            size: 4096,
            alignment: 4,
            memory_type_bits: 1,
        };
    }
}
extern "system" fn bind_buffer(
    _: vk::Device,
    _: vk::Buffer,
    _: vk::DeviceMemory,
    _: vk::DeviceSize,
) -> vk::Result {
    vk::Result::SUCCESS
}
extern "system" fn map_memory(
    _: vk::Device,
    _: vk::DeviceMemory,
    _: vk::DeviceSize,
    _: vk::DeviceSize,
    _: vk::MemoryMapFlags,
    _: *mut *mut c_void,
) -> vk::Result {
    DRIVER.with(|driver| driver.borrow().map)
}
extern "system" fn allocate_commands(
    _: vk::Device,
    _: *const vk::CommandBufferAllocateInfo<'_>,
    out: *mut vk::CommandBuffer,
) -> vk::Result {
    record("allocate command 70".into());
    unsafe {
        *out = vk::CommandBuffer::from_raw(70);
    }
    vk::Result::SUCCESS
}
extern "system" fn begin_command(
    _: vk::CommandBuffer,
    _: *const vk::CommandBufferBeginInfo<'_>,
) -> vk::Result {
    vk::Result::SUCCESS
}
fn device() -> ash::Device {
    DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
    unsafe {
        ash::Device::load_with(
            |name| match name.to_bytes() {
                b"vkCreateFence" => create_fence as *const c_void,
                b"vkQueueSubmit" => submit as *const c_void,
                b"vkWaitForFences" => wait as *const c_void,
                b"vkDestroyFence" => destroy_fence as *const c_void,
                b"vkDestroySemaphore" => destroy_semaphore as *const c_void,
                b"vkDestroyBuffer" => destroy_buffer as *const c_void,
                b"vkFreeMemory" => free_memory as *const c_void,
                b"vkDestroyImage" => destroy_image as *const c_void,
                b"vkDestroyImageView" => destroy_view as *const c_void,
                b"vkDestroyFramebuffer" => destroy_framebuffer as *const c_void,
                b"vkFreeCommandBuffers" => free_commands as *const c_void,
                b"vkFreeDescriptorSets" => free_descriptors as *const c_void,
                b"vkDestroyPipeline" => destroy_pipeline as *const c_void,
                b"vkDestroyRenderPass" => destroy_pass as *const c_void,
                b"vkDestroyShaderModule" => destroy_shader as *const c_void,
                b"vkDestroySampler" => destroy_sampler as *const c_void,
                b"vkDestroyPipelineLayout" => destroy_pipeline_layout as *const c_void,
                b"vkDestroyDescriptorPool" => destroy_descriptor_pool as *const c_void,
                b"vkDestroyDescriptorSetLayout" => destroy_descriptor_layout as *const c_void,
                b"vkDestroyCommandPool" => destroy_command_pool as *const c_void,
                b"vkDestroyDevice" => destroy_device as *const c_void,
                b"vkCreateImage" => create_image as *const c_void,
                b"vkGetImageMemoryRequirements" => image_requirements as *const c_void,
                b"vkAllocateMemory" => allocate_memory as *const c_void,
                b"vkBindImageMemory" => bind_image as *const c_void,
                b"vkCreateImageView" => create_view as *const c_void,
                b"vkCreateBuffer" => create_buffer as *const c_void,
                b"vkGetBufferMemoryRequirements" => buffer_requirements as *const c_void,
                b"vkBindBufferMemory" => bind_buffer as *const c_void,
                b"vkMapMemory" => map_memory as *const c_void,
                b"vkAllocateCommandBuffers" => allocate_commands as *const c_void,
                b"vkBeginCommandBuffer" => begin_command as *const c_void,
                _ => std::ptr::null(),
            },
            vk::Device::null(),
        )
    }
}
fn renderer(device: &ash::Device) -> AshDrmRenderer {
    let entry = Entry::from_parts_1_1(
        ash::StaticFn::load(|_| std::ptr::null()),
        ash::EntryFnV1_0::load(|_| std::ptr::null()),
        ash::EntryFnV1_1::load(|_| std::ptr::null()),
    );
    let instance = unsafe {
        ash::Instance::load_with(
            |name| match name.to_bytes() {
                b"vkGetDeviceProcAddr" => get_device_proc as *const c_void,
                b"vkDestroyInstance" => destroy_instance as *const c_void,
                _ => std::ptr::null(),
            },
            vk::Instance::null(),
        )
    };
    let external_memory_fd = khr::external_memory_fd::Device::new(&instance, device);
    let external_semaphore_fd = khr::external_semaphore_fd::Device::new(&instance, device);
    let mut memory_properties = vk::PhysicalDeviceMemoryProperties {
        memory_type_count: 1,
        memory_heap_count: 1,
        ..Default::default()
    };
    memory_properties.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL
        | vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT;
    memory_properties.memory_heaps[0].size = 1_000_000;
    AshDrmRenderer {
        _entry: entry,
        instance,
        physical_device: vk::PhysicalDevice::null(),
        device: device.clone(),
        queue: vk::Queue::null(),
        queue_family: 0,
        memory_properties,
        external_memory_fd,
        external_semaphore_fd,
        command_pool: vk::CommandPool::null(),
        descriptor_pool: vk::DescriptorPool::null(),
        descriptor_layout: vk::DescriptorSetLayout::null(),
        output_descriptor_layout: vk::DescriptorSetLayout::null(),
        solid_layout: vk::PipelineLayout::null(),
        texture_layout: vk::PipelineLayout::null(),
        output_layout: vk::PipelineLayout::null(),
        sampler: vk::Sampler::null(),
        pipelines: HashMap::new(),
        targets: HashMap::new(),
        linear_scenes: HashMap::new(),
        output_resources: HashMap::new(),
        texture_cache: HashMap::new(),
        borrowed_textures: HashMap::new(),
        retired_borrowed_descriptors: Vec::new(),
        frame_no: 0,
        pending: Vec::new(),
        completed_captures: VecDeque::new(),
        damage_history: HashMap::new(),
        stream_frames: HashMap::new(),
        output_fingerprints: HashMap::new(),
        abandon_on_drop: false,
        device_destroyed: false,
        quiesced: false,
        info: RendererInfo {
            api: crate::GraphicsApi::Vulkan,
            adapter_name: "fixture".into(),
            driver: String::new(),
            driver_info: String::new(),
            surface_format: String::new(),
        },
    }
}
fn events() -> Vec<String> {
    DRIVER.with(|driver| driver.borrow().events.clone())
}

#[test]
fn bounded_queue_drain_orders_wait_before_free_and_does_not_leak_across_resumes() {
    let device = device();
    for _ in 0..64 {
        quiesce_queue(&device, vk::Queue::null(), Duration::from_millis(125)).unwrap();
    }
    assert_eq!(events().len(), 64 * 4);
    for sequence in events().chunks(4) {
        assert_eq!(
            sequence,
            &["create fence", "submit 0", "wait 125000000", "fence 99"]
        );
    }
}
#[test]
fn injected_wait_timeout_keeps_queued_fence_alive_and_returns_promptly() {
    let device = device();
    DRIVER.with(|driver| driver.borrow_mut().wait = vk::Result::TIMEOUT);
    let error = quiesce_queue(&device, vk::Queue::null(), Duration::from_millis(20)).unwrap_err();
    assert!(!AshDrmRenderer::is_allocation_pressure(&error));
    assert_eq!(events(), ["create fence", "submit 0", "wait 20000000"]);
}
#[test]
fn injected_allocation_failures_before_submit_are_not_device_loss() {
    let device = device();
    DRIVER.with(|driver| driver.borrow_mut().create = vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
    let error = quiesce_queue(&device, vk::Queue::null(), Duration::from_secs(1)).unwrap_err();
    assert!(AshDrmRenderer::is_allocation_pressure(&error));
    assert_eq!(events(), ["create fence"]);
    let device = self::device();
    DRIVER.with(|driver| driver.borrow_mut().submit = vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    let error = quiesce_queue(&device, vk::Queue::null(), Duration::from_secs(1)).unwrap_err();
    assert!(AshDrmRenderer::is_allocation_pressure(&error));
    assert_eq!(events(), ["create fence", "submit 0", "fence 99"]);
    assert!(!AshDrmRenderer::is_allocation_pressure(
        &anyhow!(vk::Result::ERROR_DEVICE_LOST).context("submit")
    ));
}
#[test]
fn partial_frame_builds_release_each_temporary_exactly_once() {
    // Fail after each acquired resource, including a vertex buffer before
    // index-buffer allocation and a readback buffer before queue submission.
    for stage in 0..=7 {
        let device = device();
        let mut build = FrameBuild::default();
        let mut expected = Vec::new();
        if stage >= 1 {
            build.command = vk::CommandBuffer::from_raw(1);
            expected.push("command 1");
        }
        if stage >= 2 {
            build.staging.push(BufferResource {
                buffer: vk::Buffer::from_raw(2),
                memory: vk::DeviceMemory::from_raw(3),
            });
            expected.extend(["buffer 2", "memory 3"]);
        }
        if stage >= 3 {
            build.framebuffers.push(vk::Framebuffer::from_raw(4));
            expected.push("framebuffer 4");
        }
        if stage >= 4 {
            build.capture = Some(PendingCapture {
                id: 1,
                buffer: BufferResource {
                    buffer: vk::Buffer::from_raw(5),
                    memory: vk::DeviceMemory::from_raw(6),
                },
                byte_len: 4,
                width: 1,
                height: 1,
                fourcc: XRGB8888,
                output_transfer: AshDrmTransfer::Srgb,
                reference_white_nits: 203.0,
            });
            expected.extend(["buffer 5", "memory 6"]);
        }
        if stage >= 5 {
            build.wait_semaphores.push(vk::Semaphore::from_raw(7));
            expected.push("semaphore 7");
        }
        if stage >= 6 {
            build.semaphore = vk::Semaphore::from_raw(8);
            expected.push("semaphore 8");
        }
        if stage >= 7 {
            build.fence = vk::Fence::from_raw(9);
            expected.push("fence 9");
        }
        destroy_frame(
            &device,
            vk::CommandPool::null(),
            vk::DescriptorPool::null(),
            build.into_pending(1),
        );
        let mut actual = events();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "failed stage {stage}");
    }
}
#[test]
fn submitted_batch_transfers_ownership_and_is_not_freed_by_failed_export_cleanup() {
    let device = device();
    let mut build = FrameBuild {
        command: vk::CommandBuffer::from_raw(1),
        fence: vk::Fence::from_raw(2),
        ..Default::default()
    };
    let pending = std::mem::take(&mut build).into_pending(1);
    destroy_frame(
        &device,
        vk::CommandPool::null(),
        vk::DescriptorPool::null(),
        build.into_pending(2),
    );
    assert!(
        events().is_empty(),
        "rollback must not free submitted handles"
    );
    destroy_frame(
        &device,
        vk::CommandPool::null(),
        vk::DescriptorPool::null(),
        pending,
    );
    assert_eq!(events(), ["fence 2", "command 1"]);
}
#[test]
fn partial_pipeline_build_releases_pipelines_before_render_passes() {
    let device = device();
    {
        let mut build = PipelineBuild::new(&device);
        build.render_passes.push(vk::RenderPass::from_raw(1));
        build.pipelines.push(vk::Pipeline::from_raw(2));
        build.shaders.push(vk::ShaderModule::from_raw(3));
    }
    assert_eq!(events(), ["pipeline 2", "shader 3", "pass 1"]);
}
#[test]
fn repeated_device_recreation_frees_old_device_before_allocating_replacement() {
    let device = device();
    let mut current = renderer(&device);
    for _ in 0..64 {
        current.quiesce(Duration::from_millis(100)).unwrap();
        current
            .recreate_with(|| {
                record("create replacement".into());
                Ok(renderer(&device))
            })
            .unwrap();
    }
    drop(current);
    let history = events();
    assert_eq!(
        history
            .iter()
            .filter(|event| *event == "destroy device")
            .count(),
        65
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| *event == "destroy instance")
            .count(),
        65
    );
    for (index, event) in history.iter().enumerate() {
        if event == "create replacement" {
            assert_eq!(
                &history[index - 2..index],
                &["destroy device", "destroy instance"]
            );
        }
    }
}
#[test]
fn failed_replacement_is_not_retried_against_destroyed_device() {
    let device = device();
    let mut current = renderer(&device);
    current.quiesce(Duration::from_millis(100)).unwrap();
    assert!(current
        .recreate_with(|| Err(anyhow!(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)))
        .is_err());
    assert!(!current.is_usable());
    assert!(current
        .recreate_with(|| panic!("must not allocate another device"))
        .is_err());
    drop(current);
    assert_eq!(
        events()
            .iter()
            .filter(|event| *event == "destroy device")
            .count(),
        1
    );
}
#[test]
fn hung_device_latches_process_exit_and_never_attempts_destruction_or_replacement() {
    let device = device();
    let mut current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().wait = vk::Result::TIMEOUT);
    assert!(current.quiesce(Duration::from_millis(10)).is_err());
    let before = events();
    assert!(!current.is_usable());
    assert!(current
        .recreate_with(|| panic!("must not replace hung device"))
        .is_err());
    drop(current);
    assert_eq!(events(), before);
}
#[test]
fn keeping_a_wallpaper_resident_refreshes_only_an_uploaded_texture() {
    let device = device();
    let mut current = renderer(&device);
    current.frame_no = 121;
    current.texture_cache.insert(
        1,
        CachedTexture {
            resource: TextureResource::null(),
            width: 1,
            height: 1,
            format: FramePixelFormat::Bgra8Srgb,
            modifier: None,
            last_used_frame: 0,
            uploaded: false,
        },
    );

    assert!(!current.keep_uploaded_texture(2));
    assert!(!current.keep_uploaded_texture(1));
    assert_eq!(current.texture_cache[&1].last_used_frame, 0);

    current.texture_cache.get_mut(&1).unwrap().uploaded = true;
    assert!(current.keep_uploaded_texture(1));
    assert_eq!(current.texture_cache[&1].last_used_frame, 121);
}

#[test]
fn cache_reclamation_does_not_destroy_resources_referenced_by_queued_frames() {
    let device = device();
    let mut current = renderer(&device);
    current.texture_cache.insert(
        1,
        CachedTexture {
            resource: TextureResource {
                image: ImageResource {
                    image: vk::Image::from_raw(1),
                    memory: vk::DeviceMemory::from_raw(2),
                    view: vk::ImageView::from_raw(3),
                },
                descriptor_set: vk::DescriptorSet::from_raw(4),
                foreign: false,
            },
            width: 1,
            height: 1,
            format: FramePixelFormat::Bgra8Srgb,
            modifier: None,
            last_used_frame: 0,
            uploaded: false,
        },
    );
    current.pending.push(FrameBuild::default().into_pending(1));
    assert_eq!(current.reclaim_allocation_pressure(), None);
    assert!(events().is_empty());
    current.pending.clear();
    assert_eq!(current.reclaim_allocation_pressure(), Some(1));
    assert_eq!(events(), ["descriptor 4", "view 3", "image 1", "memory 2"]);
    assert!(current.texture_cache.is_empty());
}
#[test]
fn failed_frame_retirement_stays_owned_by_the_previous_submission_fence() {
    let device = device();
    let mut current = renderer(&device);
    current.pending.push(
        FrameBuild {
            fence: vk::Fence::from_raw(10),
            ..Default::default()
        }
        .into_pending(1),
    );
    let mut failed = FrameBuild::default();
    failed.retired_textures.push(TextureResource {
        image: ImageResource {
            image: vk::Image::from_raw(1),
            memory: vk::DeviceMemory::from_raw(2),
            view: vk::ImageView::from_raw(3),
        },
        descriptor_set: vk::DescriptorSet::from_raw(4),
        foreign: false,
    });
    current.rollback_frame(failed);
    assert!(events().is_empty());
    assert_eq!(current.pending[0].retired_textures.len(), 1);
    let completed = current.pending.pop().unwrap();
    current.destroy_pending(completed);
    assert_eq!(
        events(),
        ["descriptor 4", "view 3", "image 1", "memory 2", "fence 10"]
    );
}
#[test]
fn injected_owned_image_allocation_and_view_failures_leave_no_partial_image() {
    let device = device();
    let current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().fail_allocation = 1);
    let error = current
        .create_owned_image(
            4,
            4,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::SAMPLED,
        )
        .err()
        .unwrap();
    assert!(AshDrmRenderer::is_allocation_pressure(&error));
    assert!(format!("{error:#}").contains("requested_bytes=4096"));
    assert_eq!(events(), ["create image 50", "allocate 1", "image 50"]);
    drop(current);
    let device = self::device();
    let current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().view = vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
    assert!(current
        .create_owned_image(
            4,
            4,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::SAMPLED
        )
        .is_err());
    assert_eq!(
        events(),
        ["create image 50", "allocate 1", "image 50", "memory 201"]
    );
}
#[test]
fn injected_staging_oom_releases_new_texture_and_lut_images_before_retry() {
    let device = device();
    let mut current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().fail_allocation = 2);
    let mut staging = Vec::new();
    let error = current
        .prepare_output_resources(
            vk::CommandBuffer::null(),
            1,
            vk::Image::null(),
            vk::ImageView::null(),
            AshDrmOutputLut {
                grid_size: 2,
                rgb: &identity_output_lut(),
            },
            0,
            &mut staging,
        )
        .err()
        .unwrap();
    assert!(AshDrmRenderer::is_allocation_pressure(&error));
    assert!(staging.is_empty());
    assert!(current.output_resources.is_empty());
    assert_eq!(
        events(),
        [
            "create image 50",
            "allocate 1",
            "create buffer 60",
            "allocate 2",
            "buffer 60",
            "view 51",
            "image 50",
            "memory 201"
        ]
    );
    drop(current);
    let device = self::device();
    let current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().fail_allocation = 2);
    let texture = TextureQuad {
        cache_key: 1,
        pixels: vec![0, 0, 0, 255],
        width: 1,
        height: 1,
        stride: 4,
        format: FramePixelFormat::Bgra8Srgb,
        color_transform: Default::default(),
        destination: [0, 0, 1, 1],
        source_uv: [0.0, 0.0, 1.0, 1.0],
        tint: [1.0; 4],
        transform: FrameTransform::Normal,
        damage: vec![[0, 0, 1, 1]],
        dmabuf: None,
        retention: None,
    };
    assert!(current
        .prepare_texture(vk::CommandBuffer::null(), &texture, &mut staging)
        .is_err());
    assert!(staging.is_empty());
    assert_eq!(
        events(),
        [
            "create image 50",
            "allocate 1",
            "create buffer 60",
            "allocate 2",
            "buffer 60",
            "view 51",
            "image 50",
            "memory 201"
        ]
    );
}
#[test]
fn injected_buffer_allocation_and_mapping_failures_release_owned_memory() {
    let device = device();
    let current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().fail_allocation = 1);
    assert!(current.create_readback_buffer(4).is_err());
    assert_eq!(events(), ["create buffer 60", "allocate 1", "buffer 60"]);
    drop(current);
    let device = self::device();
    let current = renderer(&device);
    DRIVER.with(|driver| driver.borrow_mut().map = vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    assert!(current
        .create_host_buffer(&[0; 4], vk::BufferUsageFlags::VERTEX_BUFFER)
        .is_err());
    assert_eq!(
        events(),
        ["create buffer 60", "allocate 1", "buffer 60", "memory 201"]
    );
}
#[test]
fn real_render_build_oom_preserves_previous_target_and_device_and_cleans_scratch() {
    use std::os::fd::OwnedFd;
    let device = device();
    let mut current = renderer(&device);
    let target = DrmRenderTarget {
        width: 1,
        height: 1,
        fourcc: XRGB8888,
        modifier: 0,
        planes: vec![Arc::new(OwnedFd::from(
            std::fs::File::open("/dev/null").unwrap(),
        ))],
        offsets: vec![0],
        strides: vec![4],
    };
    let key = target_key(&target).unwrap();
    current.targets.insert(
        key,
        ImportedTarget {
            image: ImageResource {
                image: vk::Image::from_raw(10),
                memory: vk::DeviceMemory::from_raw(11),
                view: vk::ImageView::from_raw(12),
            },
            initialized: true,
            last_frame: 0,
            last_used_frame: 0,
        },
    );
    current.pipelines.insert(
        vk_format(XRGB8888).unwrap(),
        Pipelines {
            scene_render_pass: vk::RenderPass::null(),
            output_render_pass: vk::RenderPass::null(),
            solid: vk::Pipeline::null(),
            texture: vk::Pipeline::null(),
            mesh: vk::Pipeline::null(),
            output: vk::Pipeline::null(),
        },
    );
    current.linear_scenes.insert(
        1,
        LinearScene {
            resource: TextureResource {
                image: ImageResource {
                    image: vk::Image::from_raw(20),
                    memory: vk::DeviceMemory::from_raw(21),
                    view: vk::ImageView::from_raw(22),
                },
                descriptor_set: vk::DescriptorSet::from_raw(23),
                foreign: false,
            },
            width: 1,
            height: 1,
            initialized: true,
            color_fingerprint: 0,
        },
    );
    current.output_resources.insert(
        1,
        OutputResources {
            lut: ImageResource {
                image: vk::Image::from_raw(30),
                memory: vk::DeviceMemory::from_raw(31),
                view: vk::ImageView::from_raw(32),
            },
            descriptor_set: vk::DescriptorSet::from_raw(33),
            scene_image: vk::Image::from_raw(20),
            fingerprint: output_lut_fingerprint(AshDrmOutputLut {
                grid_size: 2,
                rgb: &identity_output_lut(),
            }),
            grid_size: 2,
            uploaded: true,
        },
    );
    let texture = TextureQuad {
        cache_key: 1,
        pixels: vec![0, 0, 0, 255],
        width: 1,
        height: 1,
        stride: 4,
        format: FramePixelFormat::Bgra8Srgb,
        color_transform: Default::default(),
        dmabuf: None,
        damage: vec![[0, 0, 1, 1]],
        destination: [0, 0, 1, 1],
        source_uv: [0.0, 0.0, 1.0, 1.0],
        transform: FrameTransform::Normal,
        tint: [1.0; 4],
        retention: None,
    };
    DRIVER.with(|driver| driver.borrow_mut().fail_allocation = 2);
    let error = current
        .render(
            &target,
            &[],
            &[texture],
            &[],
            0,
            &[],
            0,
            &[],
            &[],
            0,
            &[[0, 0, 1, 1]],
            1,
            None,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            AshDrmTransfer::Srgb,
            None,
            None,
        )
        .unwrap_err();
    assert!(AshDrmRenderer::is_allocation_pressure(&error));
    assert!(current.is_usable());
    assert!(current.targets[&key].initialized);
    assert!(current.texture_cache.is_empty());
    assert!(current.pending.is_empty());
    assert_eq!(
        events(),
        [
            "allocate command 70",
            "create image 50",
            "allocate 1",
            "create buffer 60",
            "allocate 2",
            "buffer 60",
            "view 51",
            "image 50",
            "memory 201",
            "command 70"
        ]
    );
}

#[test]
#[ignore = "requires explicitly selected software Vulkan ICD; never uses a display or KMS"]
fn software_vulkan_repeated_queue_lifecycle() {
    // Exercise real Vulkan dispatch on a CPU driver without touching the live GPU.
    let entry = unsafe { Entry::load() }.unwrap();
    let application = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
    let instance = unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::default().application_info(&application),
            None,
        )
    }
    .unwrap();
    let physical = unsafe { instance.enumerate_physical_devices() }.unwrap().into_iter().find(|physical| unsafe { instance.get_physical_device_properties(*physical) }.device_type == vk::PhysicalDeviceType::CPU).expect("select a CPU Vulkan ICD explicitly with VK_ICD_FILENAMES");
    let family = unsafe { instance.get_physical_device_queue_family_properties(physical) }
        .iter()
        .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .unwrap() as u32;
    let priority = [1.0];
    let queue = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(family)
        .queue_priorities(&priority)];
    for _ in 0..32 {
        let device = unsafe {
            instance.create_device(
                physical,
                &vk::DeviceCreateInfo::default().queue_create_infos(&queue),
                None,
            )
        }
        .unwrap();
        let graphics = unsafe { device.get_device_queue(family, 0) };
        quiesce_queue(&device, graphics, Duration::from_secs(1)).unwrap();
        unsafe {
            device.destroy_device(None);
        }
    }
    unsafe {
        instance.destroy_instance(None);
    }
}

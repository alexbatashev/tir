//! Independently versioned execution-provider ABI. The first provider is linked Vulkan.

use super::*;

const PLUGIN_ABI_VERSION: u32 = 1;
const SERVICES_ABI_VERSION: u32 = 1;

#[repr(C)]
pub struct PluginServices {
    struct_size: u32,
    abi_version: u32,
    context: *mut c_void,
    report_error: Option<unsafe extern "C" fn(*mut c_void, i32, i32, *const c_char, u32)>,
}

#[repr(C)]
struct PluginDeviceInfo {
    struct_size: u32,
    abi_version: u32,
    backend: u32,
    api_version: u32,
    vendor_id: u32,
    device_id: u32,
    device_type: u32,
    driver_id: u32,
    strict_profile_supported: u32,
    queue_family_count: u32,
    memory_heap_count: u32,
    cooperative_matrix_count: u32,
    features: u64,
    external_host_alignment: u64,
    cooperative_matrix_revision: u32,
    shader_bfloat16_revision: u32,
    shader_enqueue_revision: u32,
    cooperative_matrix_stages: u32,
    max_grid: [u32; 3],
    peer_memory_fd: u32,
    peer_timeline_fd: u32,
    timestamp_period_ns: f32,
    name: [c_char; 256],
    device_uuid: [u8; 16],
    driver_uuid: [u8; 16],
    driver_name: [c_char; 256],
}

#[repr(C)]
struct PluginQueueFamilyInfo {
    flags: u32,
    count: u32,
    timestamp_valid_bits: u32,
}

#[repr(C)]
struct PluginMemoryHeapInfo {
    bytes: u64,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct PluginQueueRequest {
    family: u32,
    count: u32,
    priority: f32,
}

#[repr(C)]
struct PluginAllocRequest {
    bytes: u64,
    alignment: u64,
    required_properties: u32,
    preferred_properties: u32,
    required_heap: u32,
    preferred_heap: u32,
    exclusive: u32,
    queue_family_count: u32,
    queue_families: *const u32,
}

#[repr(C)]
struct PluginAllocationInfo {
    base: u64,
    bytes: u64,
    backing_bytes: u64,
    heap_bytes: u64,
    memory_type_index: u32,
    heap_index: u32,
    properties: u32,
    exclusive: u32,
}

type InstanceOpen = unsafe extern "C" fn(*const PluginServices, *mut *mut c_void) -> i32;
type InstanceClose = unsafe extern "C" fn(*mut c_void);
type DeviceCount = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
type DeviceInfoQuery = unsafe extern "C" fn(*mut c_void, u32, *mut PluginDeviceInfo) -> i32;
type QueueFamilyQuery =
    unsafe extern "C" fn(*mut c_void, u32, u32, *mut PluginQueueFamilyInfo) -> i32;
type MemoryHeapQuery =
    unsafe extern "C" fn(*mut c_void, u32, u32, *mut PluginMemoryHeapInfo) -> i32;
type MatrixQuery = unsafe extern "C" fn(*mut c_void, u32, u32, *mut CooperativeMatrixTuple) -> i32;
type DeviceOpen =
    unsafe extern "C" fn(*mut c_void, u32, u32, *const PluginQueueRequest, *mut *mut c_void) -> i32;
type DeviceClose = unsafe extern "C" fn(*mut c_void);
type AllocationCreate = unsafe extern "C" fn(
    *mut c_void,
    *const PluginAllocRequest,
    *mut *mut c_void,
    *mut PluginAllocationInfo,
) -> i32;
type AllocationDestroy = unsafe extern "C" fn(*mut c_void, *mut c_void);
type AllocationMap = unsafe extern "C" fn(*mut c_void, *mut c_void, u64, *mut *mut c_void) -> i32;
type AllocationUnmap = unsafe extern "C" fn(*mut c_void, *mut c_void);
type AllocationVisibility = unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> i32;

#[repr(C)]
struct PluginApi {
    struct_size: u32,
    abi_version: u32,
    instance_open: Option<InstanceOpen>,
    instance_close: Option<InstanceClose>,
    device_count: Option<DeviceCount>,
    device_info: Option<DeviceInfoQuery>,
    queue_family_info: Option<QueueFamilyQuery>,
    memory_heap_info: Option<MemoryHeapQuery>,
    cooperative_matrix: Option<MatrixQuery>,
    device_open: Option<DeviceOpen>,
    device_close: Option<DeviceClose>,
    allocation_create: Option<AllocationCreate>,
    allocation_destroy: Option<AllocationDestroy>,
    allocation_map: Option<AllocationMap>,
    allocation_unmap: Option<AllocationUnmap>,
    allocation_visibility: Option<AllocationVisibility>,
}

struct VulkanInstance {
    instance: vulkan::Instance,
    devices: Vec<vulkan::DeviceRecord>,
}

fn valid_services(services: *const PluginServices) -> bool {
    if services.is_null() {
        return false;
    }
    let services = unsafe { &*services };
    services.struct_size as usize >= std::mem::size_of::<PluginServices>()
        && services.abi_version == SERVICES_ABI_VERSION
}

fn report(services: *const PluginServices, status: i32, message: &CStr) {
    let services = unsafe { &*services };
    if let Some(callback) = services.report_error {
        unsafe {
            callback(
                services.context,
                status,
                0,
                message.as_ptr(),
                message.to_bytes().len() as u32,
            )
        };
    }
}

/// Negotiates the linked Vulkan execution provider's independent table.
///
/// # Safety
/// `services` must be readable and `table` must hold `table_size` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_plugin_get_api(
    abi_version: u32,
    services: *const PluginServices,
    table_size: u32,
    table: *mut c_void,
) -> i32 {
    if table.is_null() || table_size < 8 {
        return INVALID_ARGUMENT;
    }
    let provider_size = std::mem::size_of::<PluginApi>();
    unsafe {
        ptr::write_bytes(
            table.cast::<u8>(),
            0,
            (table_size as usize).min(provider_size),
        )
    };
    if abi_version != PLUGIN_ABI_VERSION {
        return UNSUPPORTED;
    }
    if !valid_services(services) {
        return INVALID_ARGUMENT;
    }
    unsafe {
        ptr::write(table.cast::<u32>(), provider_size as u32);
        ptr::write(table.cast::<u32>().add(1), PLUGIN_ABI_VERSION);
    }
    if (table_size as usize) < provider_size {
        return INCOMPLETE;
    }
    unsafe {
        let api = &mut *table.cast::<PluginApi>();
        api.instance_open = Some(instance_open);
        api.instance_close = Some(instance_close);
        api.device_count = Some(device_count);
        api.device_info = Some(device_info);
        api.queue_family_info = Some(queue_family_info);
        api.memory_heap_info = Some(memory_heap_info);
        api.cooperative_matrix = Some(cooperative_matrix);
        api.device_open = Some(device_open);
        api.device_close = Some(device_close);
        api.allocation_create = Some(allocation_create);
        api.allocation_destroy = Some(allocation_destroy);
        api.allocation_map = Some(allocation_map);
        api.allocation_unmap = Some(allocation_unmap);
        api.allocation_visibility = Some(allocation_visibility);
    }
    SUCCESS
}

unsafe extern "C" fn instance_open(services: *const PluginServices, out: *mut *mut c_void) -> i32 {
    if !valid_services(services) || out.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let Ok(instance) = vulkan::Instance::open() else {
        report(services, NO_DEVICE, c"Vulkan loader unavailable");
        return NO_DEVICE;
    };
    let Ok(devices) = instance.devices() else {
        report(services, DEVICE_LOST, c"Vulkan device enumeration failed");
        return DEVICE_LOST;
    };
    let owner = Box::new(VulkanInstance { instance, devices });
    unsafe { *out = Box::into_raw(owner).cast() };
    SUCCESS
}

unsafe extern "C" fn instance_close(instance: *mut c_void) {
    if !instance.is_null() {
        unsafe { drop(Box::from_raw(instance.cast::<VulkanInstance>())) };
    }
}

unsafe extern "C" fn device_count(instance: *mut c_void, out: *mut u32) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Ok(count) = u32::try_from(owner.devices.len()) else {
        return INVALID_ARGUMENT;
    };
    unsafe { *out = count };
    SUCCESS
}

unsafe extern "C" fn device_info(
    instance: *mut c_void,
    index: u32,
    out: *mut PluginDeviceInfo,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let out = unsafe { &mut *out };
    if (out.struct_size as usize) < std::mem::size_of::<PluginDeviceInfo>()
        || out.abi_version != PLUGIN_ABI_VERSION
    {
        return INVALID_ARGUMENT;
    }
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Some(record) = owner.devices.get(index as usize) else {
        return INVALID_ARGUMENT;
    };
    out.backend = BACKEND_VULKAN;
    out.api_version = record.api_version;
    out.vendor_id = record.vendor_id;
    out.device_id = record.device_id;
    out.device_type = record.device_type;
    out.driver_id = record.driver_id;
    out.strict_profile_supported = u32::from(record.strict_profile_supported);
    out.queue_family_count = record.queue_families.len() as u32;
    out.memory_heap_count = record.memory_heaps.len() as u32;
    out.cooperative_matrix_count = record.cooperative_matrices.len() as u32;
    out.features = record.features;
    out.external_host_alignment = record.external_host_alignment.unwrap_or(0);
    out.cooperative_matrix_revision = record.cooperative_matrix_revision;
    out.shader_bfloat16_revision = record.shader_bfloat16_revision;
    out.shader_enqueue_revision = record.shader_enqueue_revision;
    out.cooperative_matrix_stages = record.cooperative_matrix_stages;
    out.max_grid = record.max_grid;
    out.peer_memory_fd = u32::from(record.peer_memory_fd);
    out.peer_timeline_fd = u32::from(record.peer_timeline_fd);
    out.timestamp_period_ns = record.timestamp_period_ns;
    out.name = record.name;
    out.device_uuid = record.device_uuid;
    out.driver_uuid = record.driver_uuid;
    out.driver_name = record.driver_name;
    SUCCESS
}

unsafe extern "C" fn queue_family_info(
    instance: *mut c_void,
    device_index: u32,
    family: u32,
    out: *mut PluginQueueFamilyInfo,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Some(queue) = owner
        .devices
        .get(device_index as usize)
        .and_then(|device| device.queue_families.get(family as usize))
    else {
        return INVALID_ARGUMENT;
    };
    unsafe {
        *out = PluginQueueFamilyInfo {
            flags: queue.flags,
            count: queue.count,
            timestamp_valid_bits: queue.timestamp_valid_bits,
        }
    };
    SUCCESS
}

unsafe extern "C" fn memory_heap_info(
    instance: *mut c_void,
    device_index: u32,
    heap: u32,
    out: *mut PluginMemoryHeapInfo,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Some(&(bytes, flags)) = owner
        .devices
        .get(device_index as usize)
        .and_then(|device| device.memory_heaps.get(heap as usize))
    else {
        return INVALID_ARGUMENT;
    };
    unsafe {
        *out = PluginMemoryHeapInfo {
            bytes,
            flags,
            reserved: 0,
        }
    };
    SUCCESS
}

unsafe extern "C" fn cooperative_matrix(
    instance: *mut c_void,
    device_index: u32,
    tuple: u32,
    out: *mut CooperativeMatrixTuple,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Some(&matrix) = owner
        .devices
        .get(device_index as usize)
        .and_then(|device| device.cooperative_matrices.get(tuple as usize))
    else {
        return INVALID_ARGUMENT;
    };
    unsafe { *out = matrix };
    SUCCESS
}

unsafe extern "C" fn device_open(
    instance: *mut c_void,
    index: u32,
    request_count: u32,
    requests: *const PluginQueueRequest,
    out: *mut *mut c_void,
) -> i32 {
    if instance.is_null()
        || out.is_null()
        || requests.is_null()
        || request_count == 0
        || request_count > 32
    {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let owner = unsafe { &*instance.cast::<VulkanInstance>() };
    let Some(record) = owner.devices.get(index as usize) else {
        return INVALID_ARGUMENT;
    };
    if !record.strict_profile_supported {
        return UNSUPPORTED;
    }
    let requests = unsafe { std::slice::from_raw_parts(requests, request_count as usize) };
    let mut seen = BTreeSet::new();
    let mut converted = Vec::with_capacity(requests.len());
    for request in requests {
        let Some(family) = record.queue_families.get(request.family as usize) else {
            return INVALID_ARGUMENT;
        };
        if !seen.insert(request.family)
            || request.count == 0
            || request.count > family.count
            || !request.priority.is_finite()
            || !(0.0..=1.0).contains(&request.priority)
        {
            return INVALID_ARGUMENT;
        }
        converted.push(vulkan::QueueRequest {
            family: request.family,
            count: request.count,
            priority: request.priority,
        });
    }
    let Ok(device) = owner.instance.open_device(record, &converted) else {
        return DEVICE_LOST;
    };
    unsafe { *out = Box::into_raw(Box::new(device)).cast() };
    SUCCESS
}

unsafe extern "C" fn device_close(device: *mut c_void) {
    if !device.is_null() {
        unsafe { drop(Box::from_raw(device.cast::<vulkan::Device>())) };
    }
}

unsafe extern "C" fn allocation_create(
    device: *mut c_void,
    request: *const PluginAllocRequest,
    out: *mut *mut c_void,
    info: *mut PluginAllocationInfo,
) -> i32 {
    if device.is_null() || request.is_null() || out.is_null() || info.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let request = unsafe { &*request };
    if request.bytes == 0
        || !request.alignment.is_power_of_two()
        || request.exclusive > 1
        || request.queue_family_count == 0
        || request.queue_family_count > 32
        || request.queue_families.is_null()
        || (request.required_properties | request.preferred_properties) & !7 != 0
    {
        return INVALID_ARGUMENT;
    }
    let families = unsafe {
        std::slice::from_raw_parts(request.queue_families, request.queue_family_count as usize)
    };
    if families
        .iter()
        .any(|family| !device.queues.iter().any(|queue| queue.family == *family))
    {
        return INVALID_ARGUMENT;
    }
    let required_heap = (request.required_heap != u32::MAX).then_some(request.required_heap);
    let preferred_heap = (request.preferred_heap != u32::MAX).then_some(request.preferred_heap);
    if required_heap.is_some_and(|heap| heap >= device.memory_heap_count())
        || preferred_heap.is_some_and(|heap| heap >= device.memory_heap_count())
    {
        return UNSUPPORTED;
    }
    let backend_request = vulkan::AllocRequest {
        bytes: request.bytes,
        alignment: request.alignment,
        required_properties: request.required_properties,
        preferred_properties: request.preferred_properties,
        queue_families: families,
        required_heap,
        preferred_heap,
        exclusive: request.exclusive != 0,
    };
    let allocation = match device.allocate(&backend_request) {
        Ok(allocation) => allocation,
        Err(error) => return allocation_status(error),
    };
    let (backing_bytes, heap_bytes, memory_type_index, heap_index, properties) =
        allocation.memory_info(device);
    unsafe {
        *info = PluginAllocationInfo {
            base: allocation.base,
            bytes: allocation.bytes,
            backing_bytes,
            heap_bytes,
            memory_type_index,
            heap_index,
            properties,
            exclusive: u32::from(allocation.exclusive()),
        };
        *out = Box::into_raw(Box::new(allocation)).cast();
    }
    SUCCESS
}

unsafe extern "C" fn allocation_destroy(device: *mut c_void, allocation: *mut c_void) {
    if device.is_null() || allocation.is_null() {
        return;
    }
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = unsafe { *Box::from_raw(allocation.cast::<vulkan::Allocation>()) };
    device.free_allocation(allocation);
}

unsafe extern "C" fn allocation_map(
    device: *mut c_void,
    allocation: *mut c_void,
    address: u64,
    out: *mut *mut c_void,
) -> i32 {
    if device.is_null() || allocation.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = unsafe { &mut *allocation.cast::<vulkan::Allocation>() };
    match device.map_allocation(allocation, address) {
        Ok(host) => {
            unsafe { *out = host };
            SUCCESS
        }
        Err(error) => allocation_status(error),
    }
}

unsafe extern "C" fn allocation_unmap(device: *mut c_void, allocation: *mut c_void) {
    if device.is_null() || allocation.is_null() {
        return;
    }
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = unsafe { &mut *allocation.cast::<vulkan::Allocation>() };
    device.unmap_allocation(allocation);
}

unsafe extern "C" fn allocation_visibility(
    device: *mut c_void,
    allocation: *mut c_void,
    flush: u32,
) -> i32 {
    if device.is_null() || allocation.is_null() || flush > 1 {
        return INVALID_ARGUMENT;
    }
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = unsafe { &*allocation.cast::<vulkan::Allocation>() };
    match device.visibility(allocation, flush != 0) {
        Ok(()) => SUCCESS,
        Err(error) => allocation_status(error),
    }
}

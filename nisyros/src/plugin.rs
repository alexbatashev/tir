//! Independently versioned execution-provider ABI. The first provider is linked Vulkan.

use super::*;
use std::os::fd::{FromRawFd, IntoRawFd};

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
type AllocationRegisterHost = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    u64,
    *mut *mut c_void,
    *mut PluginAllocationInfo,
) -> i32;
type AllocationExportFd = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut i32) -> i32;
type AllocationImportFd = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    u64,
    u64,
    i32,
    *mut *mut c_void,
    *mut PluginAllocationInfo,
) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
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
    allocation_register_host: Option<AllocationRegisterHost>,
    allocation_export_fd: Option<AllocationExportFd>,
    allocation_import_fd: Option<AllocationImportFd>,
}

struct VulkanInstance {
    instance: vulkan::Instance,
    devices: Vec<vulkan::DeviceRecord>,
}

#[derive(Clone, Copy)]
pub struct QueueFamilyProperties {
    pub flags: u32,
    pub count: u32,
    pub timestamp_valid_bits: u32,
}

pub struct DeviceRecord {
    pub index: u32,
    pub backend: u32,
    pub api_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: u32,
    pub driver_id: u32,
    pub strict_profile_supported: bool,
    pub name: [c_char; 256],
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    pub driver_name: [c_char; 256],
    pub queue_families: Vec<QueueFamilyProperties>,
    pub memory_heaps: Vec<(u64, u32)>,
    pub features: u64,
    pub cooperative_matrix_revision: u32,
    pub shader_bfloat16_revision: u32,
    pub shader_enqueue_revision: u32,
    pub cooperative_matrix_stages: u32,
    pub cooperative_matrices: Vec<CooperativeMatrixTuple>,
    pub timestamp_period_ns: f32,
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
        api.allocation_register_host = Some(allocation_register_host);
        api.allocation_export_fd = Some(allocation_export_fd);
        api.allocation_import_fd = Some(allocation_import_fd);
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

fn allocation_info(
    device: &vulkan::Device,
    allocation: &vulkan::Allocation,
) -> PluginAllocationInfo {
    let (backing_bytes, heap_bytes, memory_type_index, heap_index, properties) =
        allocation.memory_info(device);
    PluginAllocationInfo {
        base: allocation.base,
        bytes: allocation.bytes,
        backing_bytes,
        heap_bytes,
        memory_type_index,
        heap_index,
        properties,
        exclusive: u32::from(allocation.exclusive()),
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
    unsafe {
        *info = allocation_info(device, &allocation);
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

unsafe extern "C" fn allocation_register_host(
    device: *mut c_void,
    host: *mut c_void,
    bytes: u64,
    out: *mut *mut c_void,
    info: *mut PluginAllocationInfo,
) -> i32 {
    if device.is_null() || host.is_null() || bytes == 0 || out.is_null() || info.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = match device.register_host_pointer(host, bytes) {
        Ok(allocation) => allocation,
        Err(error) => return allocation_status(error),
    };
    unsafe {
        *info = allocation_info(device, &allocation);
        *out = Box::into_raw(Box::new(allocation)).cast();
    }
    SUCCESS
}

unsafe extern "C" fn allocation_export_fd(
    device: *mut c_void,
    allocation: *mut c_void,
    out_fd: *mut i32,
) -> i32 {
    if device.is_null() || allocation.is_null() || out_fd.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out_fd = -1 };
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let allocation = unsafe { &*allocation.cast::<vulkan::Allocation>() };
    match device.export_allocation_fd(allocation) {
        Ok(fd) => {
            unsafe { *out_fd = fd.into_raw_fd() };
            SUCCESS
        }
        Err(error) => allocation_status(error),
    }
}

unsafe extern "C" fn allocation_import_fd(
    device: *mut c_void,
    source_allocation: *mut c_void,
    source_offset: u64,
    bytes: u64,
    fd: i32,
    out: *mut *mut c_void,
    info: *mut PluginAllocationInfo,
) -> i32 {
    if fd < 0 {
        return INVALID_ARGUMENT;
    }
    // SAFETY: The plugin call takes ownership of this descriptor on every path.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if device.is_null() || source_allocation.is_null() || out.is_null() || info.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let device = unsafe { &*device.cast::<vulkan::Device>() };
    let source = unsafe { &*source_allocation.cast::<vulkan::Allocation>() };
    let allocation = match device.import_peer_fd(source, source_offset, bytes, fd) {
        Ok(allocation) => allocation,
        Err(error) => return allocation_status(error),
    };
    unsafe {
        *info = allocation_info(device, &allocation);
        *out = Box::into_raw(Box::new(allocation)).cast();
    }
    SUCCESS
}

unsafe extern "C" fn core_report_error(
    _context: *mut c_void,
    status: i32,
    backend_code: i32,
    _message: *const c_char,
    _message_bytes: u32,
) {
    set_backend_diagnostic(status, backend_code, "Execution provider failed");
}

/// Core-owned handle to the linked provider's inventory session.
pub struct Instance {
    api: PluginApi,
    handle: *mut c_void,
}

impl Instance {
    pub fn open() -> Result<Self, ()> {
        let services = PluginServices {
            struct_size: std::mem::size_of::<PluginServices>() as u32,
            abi_version: SERVICES_ABI_VERSION,
            context: ptr::null_mut(),
            report_error: Some(core_report_error),
        };
        let mut api: PluginApi = unsafe { std::mem::zeroed() };
        let status = unsafe {
            nisyros_plugin_get_api(
                PLUGIN_ABI_VERSION,
                &services,
                std::mem::size_of::<PluginApi>() as u32,
                (&raw mut api).cast(),
            )
        };
        if status != SUCCESS
            || api.instance_open.is_none()
            || api.instance_close.is_none()
            || api.device_count.is_none()
            || api.device_info.is_none()
            || api.queue_family_info.is_none()
            || api.memory_heap_info.is_none()
            || api.cooperative_matrix.is_none()
            || api.device_open.is_none()
            || api.device_close.is_none()
            || api.allocation_create.is_none()
            || api.allocation_destroy.is_none()
            || api.allocation_map.is_none()
            || api.allocation_unmap.is_none()
            || api.allocation_visibility.is_none()
            || api.allocation_register_host.is_none()
            || api.allocation_export_fd.is_none()
            || api.allocation_import_fd.is_none()
        {
            return Err(());
        }
        let mut handle = ptr::null_mut();
        if unsafe { api.instance_open.expect("checked provider callback")(&services, &mut handle) }
            != SUCCESS
            || handle.is_null()
        {
            return Err(());
        }
        Ok(Self { api, handle })
    }

    pub fn devices(&self) -> Result<Vec<DeviceRecord>, ()> {
        let mut count = 0;
        if unsafe {
            self.api.device_count.expect("checked provider callback")(self.handle, &mut count)
        } != SUCCESS
            || count > 32
        {
            return Err(());
        }
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut info: PluginDeviceInfo = unsafe { std::mem::zeroed() };
            info.struct_size = std::mem::size_of::<PluginDeviceInfo>() as u32;
            info.abi_version = PLUGIN_ABI_VERSION;
            if unsafe {
                self.api.device_info.expect("checked provider callback")(
                    self.handle,
                    index,
                    &mut info,
                )
            } != SUCCESS
                || info.backend != BACKEND_VULKAN
                || info.strict_profile_supported > 1
                || info.peer_memory_fd > 1
                || info.peer_timeline_fd > 1
                || info.queue_family_count > 32
                || info.memory_heap_count > 16
                || info.cooperative_matrix_count > 256
                || !info.name.contains(&0)
                || !info.driver_name.contains(&0)
            {
                return Err(());
            }
            let mut queue_families = Vec::with_capacity(info.queue_family_count as usize);
            for family in 0..info.queue_family_count {
                let mut queue: PluginQueueFamilyInfo = unsafe { std::mem::zeroed() };
                if unsafe {
                    self.api
                        .queue_family_info
                        .expect("checked provider callback")(
                        self.handle, index, family, &mut queue
                    )
                } != SUCCESS
                    || queue.count == 0
                    || (queue.timestamp_valid_bits != 0
                        && !(36..=64).contains(&queue.timestamp_valid_bits))
                {
                    return Err(());
                }
                queue_families.push(QueueFamilyProperties {
                    flags: queue.flags,
                    count: queue.count,
                    timestamp_valid_bits: queue.timestamp_valid_bits,
                });
            }
            let mut memory_heaps = Vec::with_capacity(info.memory_heap_count as usize);
            for heap in 0..info.memory_heap_count {
                let mut value: PluginMemoryHeapInfo = unsafe { std::mem::zeroed() };
                if unsafe {
                    self.api
                        .memory_heap_info
                        .expect("checked provider callback")(
                        self.handle, index, heap, &mut value
                    )
                } != SUCCESS
                    || value.bytes == 0
                    || value.reserved != 0
                {
                    return Err(());
                }
                memory_heaps.push((value.bytes, value.flags));
            }
            let mut cooperative_matrices =
                Vec::with_capacity(info.cooperative_matrix_count as usize);
            for tuple in 0..info.cooperative_matrix_count {
                let mut value: CooperativeMatrixTuple = unsafe { std::mem::zeroed() };
                if unsafe {
                    self.api
                        .cooperative_matrix
                        .expect("checked provider callback")(
                        self.handle, index, tuple, &mut value
                    )
                } != SUCCESS
                    || value.m == 0
                    || value.n == 0
                    || value.k == 0
                    || value.saturating_accumulation > 1
                {
                    return Err(());
                }
                cooperative_matrices.push(value);
            }
            devices.push(DeviceRecord {
                index,
                backend: info.backend,
                api_version: info.api_version,
                vendor_id: info.vendor_id,
                device_id: info.device_id,
                device_type: info.device_type,
                driver_id: info.driver_id,
                strict_profile_supported: info.strict_profile_supported != 0,
                name: info.name,
                device_uuid: info.device_uuid,
                driver_uuid: info.driver_uuid,
                driver_name: info.driver_name,
                queue_families,
                memory_heaps,
                features: info.features,
                cooperative_matrix_revision: info.cooperative_matrix_revision,
                shader_bfloat16_revision: info.shader_bfloat16_revision,
                shader_enqueue_revision: info.shader_enqueue_revision,
                cooperative_matrix_stages: info.cooperative_matrix_stages,
                cooperative_matrices,
                timestamp_period_ns: info.timestamp_period_ns,
            });
        }
        Ok(devices)
    }

    pub fn native_handle(&self) -> *mut c_void {
        unsafe { &*self.handle.cast::<VulkanInstance>() }
            .instance
            .native_handle()
    }

    pub fn native_physical_handle(&self, index: usize) -> Option<*mut c_void> {
        unsafe { &*self.handle.cast::<VulkanInstance>() }
            .devices
            .get(index)
            .map(|record| record.physical)
    }

    pub fn open_device(
        &self,
        record: &DeviceRecord,
        requests: &[vulkan::QueueRequest],
    ) -> Result<Device, ()> {
        let converted: Vec<_> = requests
            .iter()
            .map(|request| PluginQueueRequest {
                family: request.family,
                count: request.count,
                priority: request.priority,
            })
            .collect();
        let mut handle = ptr::null_mut();
        let status = unsafe {
            self.api.device_open.expect("checked provider callback")(
                self.handle,
                record.index,
                converted.len() as u32,
                converted.as_ptr(),
                &mut handle,
            )
        };
        if status != SUCCESS || handle.is_null() {
            return Err(());
        }
        Ok(Device {
            api: self.api,
            handle,
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { self.api.instance_close.expect("checked provider callback")(self.handle) };
    }
}

/// Opaque provider device. Unmigrated operations still use the linked Vulkan
/// implementation through this wrapper until the execution table covers them.
pub struct Device {
    api: PluginApi,
    handle: *mut c_void,
}

impl std::ops::Deref for Device {
    type Target = vulkan::Device;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.handle.cast::<vulkan::Device>() }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { self.api.device_close.expect("checked provider callback")(self.handle) };
    }
}

pub struct Allocation {
    api: PluginApi,
    device: *mut c_void,
    handle: *mut c_void,
    pub base: u64,
    pub bytes: u64,
    backing_bytes: u64,
    heap_bytes: u64,
    memory_type_index: u32,
    heap_index: u32,
    properties: u32,
    exclusive: bool,
    mapped: bool,
    mapped_host: *mut c_void,
}

impl Allocation {
    fn new(
        api: PluginApi,
        device: *mut c_void,
        handle: *mut c_void,
        info: PluginAllocationInfo,
        mapped_host: *mut c_void,
    ) -> Self {
        Self {
            api,
            device,
            handle,
            base: info.base,
            bytes: info.bytes,
            backing_bytes: info.backing_bytes,
            heap_bytes: info.heap_bytes,
            memory_type_index: info.memory_type_index,
            heap_index: info.heap_index,
            properties: info.properties,
            exclusive: info.exclusive != 0,
            mapped: false,
            mapped_host,
        }
    }

    pub fn contains(&self, address: u64, bytes: u64) -> bool {
        address >= self.base
            && address
                .checked_sub(self.base)
                .and_then(|offset| offset.checked_add(bytes))
                .is_some_and(|end| end <= self.bytes)
    }

    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    pub fn mapped_host(&self) -> *mut c_void {
        self.mapped_host
    }

    pub fn exclusive(&self) -> bool {
        self.exclusive
    }

    pub fn memory_info(&self, _device: &Device) -> (u64, u64, u32, u32, u32) {
        (
            self.backing_bytes,
            self.heap_bytes,
            self.memory_type_index,
            self.heap_index,
            self.properties,
        )
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            self.api
                .allocation_destroy
                .expect("checked provider callback")(self.device, self.handle)
        };
    }
}

fn allocation_error(status: i32) -> vulkan::AllocError {
    match status {
        INVALID_ARGUMENT => vulkan::AllocError::Invalid,
        UNSUPPORTED => vulkan::AllocError::Unsupported,
        OUT_OF_MEMORY => vulkan::AllocError::OutOfMemory,
        _ => vulkan::AllocError::Backend,
    }
}

impl Device {
    pub fn allocate(
        &self,
        request: &vulkan::AllocRequest<'_>,
    ) -> Result<Allocation, vulkan::AllocError> {
        let input = PluginAllocRequest {
            bytes: request.bytes,
            alignment: request.alignment,
            required_properties: request.required_properties,
            preferred_properties: request.preferred_properties,
            required_heap: request.required_heap.unwrap_or(u32::MAX),
            preferred_heap: request.preferred_heap.unwrap_or(u32::MAX),
            exclusive: u32::from(request.exclusive),
            queue_family_count: request.queue_families.len() as u32,
            queue_families: request.queue_families.as_ptr(),
        };
        let mut handle = ptr::null_mut();
        let mut info: PluginAllocationInfo = unsafe { std::mem::zeroed() };
        let status = unsafe {
            self.api
                .allocation_create
                .expect("checked provider callback")(
                self.handle, &input, &mut handle, &mut info
            )
        };
        if status != SUCCESS {
            return Err(allocation_error(status));
        }
        if handle.is_null() || info.base == 0 || info.bytes != request.bytes {
            if !handle.is_null() {
                unsafe {
                    self.api
                        .allocation_destroy
                        .expect("checked provider callback")(self.handle, handle)
                };
            }
            return Err(vulkan::AllocError::Backend);
        }
        Ok(Allocation::new(
            self.api,
            self.handle,
            handle,
            info,
            ptr::null_mut(),
        ))
    }

    pub fn register_host_pointer(
        &self,
        host: *mut c_void,
        bytes: u64,
    ) -> Result<Allocation, vulkan::AllocError> {
        let mut handle = ptr::null_mut();
        let mut info: PluginAllocationInfo = unsafe { std::mem::zeroed() };
        let status = unsafe {
            self.api
                .allocation_register_host
                .expect("checked provider callback")(
                self.handle,
                host,
                bytes,
                &mut handle,
                &mut info,
            )
        };
        if status != SUCCESS {
            return Err(allocation_error(status));
        }
        if handle.is_null() || info.base == 0 || info.bytes != bytes {
            if !handle.is_null() {
                unsafe {
                    self.api
                        .allocation_destroy
                        .expect("checked provider callback")(self.handle, handle)
                };
            }
            return Err(vulkan::AllocError::Backend);
        }
        Ok(Allocation::new(self.api, self.handle, handle, info, host))
    }

    pub fn export_allocation_fd(
        &self,
        allocation: &Allocation,
    ) -> Result<OwnedFd, vulkan::AllocError> {
        let mut fd = -1;
        let status = unsafe {
            self.api
                .allocation_export_fd
                .expect("checked provider callback")(
                self.handle, allocation.handle, &mut fd
            )
        };
        if status != SUCCESS {
            return Err(allocation_error(status));
        }
        if fd < 0 {
            return Err(vulkan::AllocError::Backend);
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub fn import_peer_fd(
        &self,
        source: &Allocation,
        source_offset: u64,
        bytes: u64,
        fd: OwnedFd,
    ) -> Result<Allocation, vulkan::AllocError> {
        let mut handle = ptr::null_mut();
        let mut info: PluginAllocationInfo = unsafe { std::mem::zeroed() };
        let status = unsafe {
            self.api
                .allocation_import_fd
                .expect("checked provider callback")(
                self.handle,
                source.handle,
                source_offset,
                bytes,
                fd.into_raw_fd(),
                &mut handle,
                &mut info,
            )
        };
        if status != SUCCESS {
            return Err(allocation_error(status));
        }
        if handle.is_null() || info.base == 0 || info.bytes != bytes {
            if !handle.is_null() {
                unsafe {
                    self.api
                        .allocation_destroy
                        .expect("checked provider callback")(self.handle, handle)
                };
            }
            return Err(vulkan::AllocError::Backend);
        }
        Ok(Allocation::new(
            self.api,
            self.handle,
            handle,
            info,
            ptr::null_mut(),
        ))
    }

    pub fn map_allocation(
        &self,
        allocation: &mut Allocation,
        address: u64,
    ) -> Result<*mut c_void, vulkan::AllocError> {
        let mut host = ptr::null_mut();
        let status = unsafe {
            self.api.allocation_map.expect("checked provider callback")(
                self.handle,
                allocation.handle,
                address,
                &mut host,
            )
        };
        if status != SUCCESS {
            return Err(allocation_error(status));
        }
        if host.is_null() {
            return Err(vulkan::AllocError::Backend);
        }
        allocation.mapped = true;
        allocation.mapped_host = host;
        Ok(host)
    }

    pub fn unmap_allocation(&self, allocation: &mut Allocation) {
        unsafe {
            self.api
                .allocation_unmap
                .expect("checked provider callback")(self.handle, allocation.handle)
        };
        allocation.mapped = false;
        allocation.mapped_host = ptr::null_mut();
    }

    pub fn visibility(
        &self,
        allocation: &Allocation,
        flush: bool,
    ) -> Result<(), vulkan::AllocError> {
        let status = unsafe {
            self.api
                .allocation_visibility
                .expect("checked provider callback")(
                self.handle,
                allocation.handle,
                u32::from(flush),
            )
        };
        if status == SUCCESS {
            Ok(())
        } else {
            Err(allocation_error(status))
        }
    }

    pub fn free_allocation(&self, allocation: Allocation) {
        debug_assert_eq!(allocation.device, self.handle);
        drop(allocation);
    }
}

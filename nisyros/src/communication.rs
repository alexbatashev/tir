//! Explicit software communication endpoints for fixed two-node groups.

use super::*;
use crate::transport::{Frame, FrameKind, MAX_MESSAGE_BYTES, MessageKey};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

const MAX_PENDING_MESSAGES: usize = 256;

#[repr(C)]
pub struct RankBinding {
    rank: u32,
    reserved: u32,
    device: *mut Device,
    transfer_queue: *mut Queue,
}

#[repr(C)]
pub struct EndpointDesc {
    header: DescriptorHeader,
    binding_count: u32,
    bindings: *const RankBinding,
    allow_host_staging: u32,
    reserved: u32,
    failure_timeout_ns: u64,
}

#[repr(C)]
pub struct EndpointInfo {
    header: DescriptorHeader,
    software_progress: u32,
    worker_threads: u32,
    uses_host_staging: u32,
    pending_limit: u32,
}

#[repr(C)]
pub struct SequenceBinding {
    slot: u32,
    reserved: u32,
    sequence: u64,
}

#[repr(C)]
pub struct CommSubmitDesc {
    header: DescriptorHeader,
    binding_count: u32,
    bindings: *const SequenceBinding,
}

#[derive(Clone, Copy)]
struct Binding {
    rank: u32,
    device: usize,
    queue: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferKind {
    Send,
    Recv,
}

#[derive(Clone, Copy)]
enum SequenceRef {
    Static(u64),
    Slot(u32),
}

#[derive(Clone, Copy)]
struct RecordedTransfer {
    kind: TransferKind,
    source_rank: u32,
    destination_rank: u32,
    channel: u64,
    sequence: SequenceRef,
    device: usize,
    queue: usize,
    address: u64,
    bytes: u64,
}

#[derive(Clone, Copy)]
struct TransferOp {
    kind: TransferKind,
    key: MessageKey,
    device: usize,
    queue: usize,
    address: u64,
    bytes: u64,
}

struct TransferRequest {
    local_rank: u32,
    peer_rank: u32,
    channel: u64,
    sequence: SequenceRef,
    address: u64,
    bytes: u64,
}

struct Task {
    value: u64,
    operations: Vec<TransferOp>,
    prerequisites: Vec<(usize, u64)>,
}

enum ProgressMessage {
    Submit(Task),
    Remote(Frame),
    PeerFailed,
    Shutdown,
}

struct EndpointState {
    next_value: u64,
    used_keys: BTreeSet<MessageKey>,
    active_keys: BTreeSet<MessageKey>,
}

struct Shared {
    completion: usize,
    failure: AtomicI32,
    pending: AtomicUsize,
    state: Mutex<EndpointState>,
}

struct ProgressConfig {
    bindings: Vec<Binding>,
    group_epoch: u64,
    rank_count: u32,
    timeout: Duration,
}

pub struct Endpoint {
    group: *mut Group,
    bindings: Vec<Binding>,
    devices: Vec<usize>,
    shared: Arc<Shared>,
    sender: mpsc::Sender<ProgressMessage>,
    worker: Mutex<Option<JoinHandle<()>>>,
    live_commands: AtomicUsize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommStateKind {
    Initial,
    Recording,
    Executable,
    Pending,
    Invalid,
}

struct CommState {
    kind: CommStateKind,
    operations: Vec<RecordedTransfer>,
    references: BTreeSet<(usize, u64)>,
    event_references: BTreeSet<usize>,
    prerequisites: Vec<Point>,
    pending: Option<Point>,
}

pub struct CommCommand {
    endpoint: *mut Endpoint,
    state: Mutex<CommState>,
}

/// Opens a host-staged communication endpoint with every local rank binding.
///
/// # Safety
/// `group` must be live, the descriptor and bindings readable, and `out`
/// writable. Bound devices and queues must outlive the endpoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_endpoint_open(
    group: *mut Group,
    desc: *const EndpointDesc,
    out: *mut *mut Endpoint,
) -> i32 {
    if group.is_null() || desc.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let group_ref = unsafe { &*group };
    let cluster = unsafe { &*group_ref.cluster };
    let desc = unsafe { &*desc };
    let status = validate_header(&desc.header, std::mem::size_of::<EndpointDesc>());
    if status != SUCCESS {
        return status;
    }
    if !cluster.distributed
        || desc.allow_host_staging != 1
        || desc.reserved != 0
        || desc.binding_count == 0
        || desc.binding_count > 64
        || desc.bindings.is_null()
        || desc.failure_timeout_ns == 0
    {
        return UNSUPPORTED;
    }
    if desc.failure_timeout_ns < 100_000_000 {
        return INVALID_ARGUMENT;
    }
    let raw_bindings =
        unsafe { std::slice::from_raw_parts(desc.bindings, desc.binding_count as usize) };
    let local_ranks: BTreeSet<u32> = group_ref
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| {
            cluster.inventory[member.index as usize]
                .local_index
                .is_some()
        })
        .map(|(rank, _)| rank as u32)
        .collect();
    if local_ranks.len() != raw_bindings.len() {
        return INVALID_ARGUMENT;
    }
    let mut seen = BTreeSet::new();
    let mut bindings = Vec::with_capacity(raw_bindings.len());
    let mut devices = BTreeSet::new();
    for binding in raw_bindings {
        if binding.reserved != 0
            || binding.device.is_null()
            || binding.transfer_queue.is_null()
            || !local_ranks.contains(&binding.rank)
            || !seen.insert(binding.rank)
        {
            return INVALID_ARGUMENT;
        }
        let device = unsafe { &*binding.device };
        let queue = unsafe { &*binding.transfer_queue };
        let member = group_ref.members[binding.rank as usize];
        if device.cluster != group_ref.cluster
            || device.inventory_index != member.index
            || queue.parent != binding.device
            || queue.family as usize
                >= cluster.inventory[member.index as usize]
                    .snapshot
                    .queue_families
                    .len()
            || cluster.inventory[member.index as usize]
                .snapshot
                .queue_families[queue.family as usize]
                .flags
                & 4
                == 0
        {
            return INVALID_ARGUMENT;
        }
        devices.insert(binding.device as usize);
        bindings.push(Binding {
            rank: binding.rank,
            device: binding.device as usize,
            queue: binding.transfer_queue as usize,
        });
    }
    let first_device = raw_bindings[0].device;
    let event_desc = EventDesc {
        header: DescriptorHeader {
            struct_size: std::mem::size_of::<EventDesc>() as u32,
            abi_version: ABI_VERSION,
            extensions: ptr::null(),
        },
        signaling_domain: 2,
        reserved: 0,
    };
    let mut completion = ptr::null_mut();
    let status = unsafe { nisyros_event_create(first_device, &event_desc, &mut completion) };
    if status != SUCCESS {
        return status;
    }
    unsafe { (*completion).requires_host_progress = true };
    let Ok(mut connection) = cluster.network.lock() else {
        unsafe { nisyros_event_destroy(completion) };
        return PEER_LOST;
    };
    let Some(network_connection) = connection.take() else {
        unsafe { nisyros_event_destroy(completion) };
        return BUSY;
    };
    drop(connection);
    let (sender, receiver) = mpsc::channel();
    let shared = Arc::new(Shared {
        completion: completion as usize,
        failure: AtomicI32::new(0),
        pending: AtomicUsize::new(0),
        state: Mutex::new(EndpointState {
            next_value: 0,
            used_keys: BTreeSet::new(),
            active_keys: BTreeSet::new(),
        }),
    });
    let worker_shared = Arc::clone(&shared);
    let worker_sender = sender.clone();
    let config = ProgressConfig {
        bindings: bindings.clone(),
        group_epoch: group_ref.epoch,
        rank_count: group_ref.members.len() as u32,
        timeout: Duration::from_nanos(desc.failure_timeout_ns),
    };
    let connection_cell = Arc::new(Mutex::new(Some(network_connection)));
    let worker_cell = Arc::clone(&connection_cell);
    let panic_shared = Arc::clone(&shared);
    let worker = match thread::Builder::new()
        .name("nisyros-progress".into())
        .spawn(move || {
            let connection = worker_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("reserved connection");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                progress_loop(
                    connection.into_stream(),
                    receiver,
                    worker_sender,
                    worker_shared,
                    config,
                )
            }));
            if result.is_err() {
                mark_failed(&panic_shared, PEER_LOST);
            }
        }) {
        Ok(worker) => worker,
        Err(_) => {
            if let Some(connection) = connection_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                cluster
                    .network
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(connection);
            }
            unsafe { nisyros_event_destroy(completion) };
            return OUT_OF_MEMORY;
        }
    };
    for &device in &devices {
        unsafe {
            (*(device as *mut Device))
                .live_endpoints
                .fetch_add(1, Ordering::Release)
        };
    }
    group_ref.live_endpoints.fetch_add(1, Ordering::Release);
    let endpoint = Box::new(Endpoint {
        group,
        bindings,
        devices: devices.into_iter().collect(),
        shared,
        sender,
        worker: Mutex::new(Some(worker)),
        live_commands: AtomicUsize::new(0),
    });
    unsafe { *out = Box::into_raw(endpoint) };
    SUCCESS
}

/// Reports progress and staging costs of a communication endpoint.
///
/// # Safety
/// `endpoint` must be live and `out` a complete initialized struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_endpoint_query(
    endpoint: *mut Endpoint,
    out: *mut EndpointInfo,
) -> i32 {
    if endpoint.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    let out = unsafe { &mut *out };
    let status = validate_header(&out.header, std::mem::size_of::<EndpointInfo>());
    if status != SUCCESS {
        return status;
    }
    out.software_progress = 1;
    out.worker_threads = 2;
    out.uses_host_staging = 1;
    out.pending_limit = MAX_PENDING_MESSAGES as u32;
    SUCCESS
}

/// Closes an idle endpoint without waiting for application work.
///
/// # Safety
/// `endpoint` must be a live handle returned by `nisyros_endpoint_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_endpoint_close(endpoint: *mut Endpoint) -> i32 {
    if endpoint.is_null() {
        return INVALID_ARGUMENT;
    }
    let endpoint_ref = unsafe { &*endpoint };
    if endpoint_ref.live_commands.load(Ordering::Acquire) != 0
        || (endpoint_ref.shared.pending.load(Ordering::Acquire) != 0
            && endpoint_ref.shared.failure.load(Ordering::Acquire) == 0)
    {
        return BUSY;
    }
    let _ = endpoint_ref.sender.send(ProgressMessage::Shutdown);
    if let Some(worker) = endpoint_ref
        .worker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        let _ = worker.join();
    }
    let completion = endpoint_ref.shared.completion as *mut Event;
    let group = endpoint_ref.group;
    let devices = endpoint_ref.devices.clone();
    let status = unsafe { nisyros_event_destroy(completion) };
    if status != SUCCESS {
        return status;
    }
    unsafe { drop(Box::from_raw(endpoint)) };
    for device in devices {
        unsafe {
            (*(device as *mut Device))
                .live_endpoints
                .fetch_sub(1, Ordering::Release)
        };
    }
    unsafe { (*group).live_endpoints.fetch_sub(1, Ordering::Release) };
    SUCCESS
}

/// Creates a communication recording for one endpoint.
///
/// # Safety
/// `endpoint` must be live and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_create(
    endpoint: *mut Endpoint,
    out: *mut *mut CommCommand,
) -> i32 {
    if endpoint.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out = ptr::null_mut() };
    let endpoint_ref = unsafe { &*endpoint };
    if endpoint_ref.shared.failure.load(Ordering::Acquire) != 0 {
        return PEER_LOST;
    }
    let cmd = Box::new(CommCommand {
        endpoint,
        state: Mutex::new(CommState {
            kind: CommStateKind::Initial,
            operations: Vec::new(),
            references: BTreeSet::new(),
            event_references: BTreeSet::new(),
            prerequisites: Vec::new(),
            pending: None,
        }),
    });
    endpoint_ref.live_commands.fetch_add(1, Ordering::Release);
    unsafe { *out = Box::into_raw(cmd) };
    SUCCESS
}

/// Begins a new communication recording.
///
/// # Safety
/// `cmd` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_begin(cmd: *mut CommCommand) -> i32 {
    if cmd.is_null() {
        return INVALID_ARGUMENT;
    }
    let Ok(mut state) = (unsafe { &*cmd }).state.lock() else {
        return PEER_LOST;
    };
    if state.kind != CommStateKind::Initial {
        return INVALID_ARGUMENT;
    }
    state.kind = CommStateKind::Recording;
    SUCCESS
}

/// Records a prerequisite event point before communication begins.
///
/// # Safety
/// `cmd` and `point.event` must be live through submission completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_wait(cmd: *mut CommCommand, point: Point) -> i32 {
    if cmd.is_null() || point.event.is_null() {
        return INVALID_ARGUMENT;
    }
    let cmd_ref = unsafe { &*cmd };
    let endpoint = unsafe { &*cmd_ref.endpoint };
    let event = unsafe { &*point.event };
    if !endpoint.devices.contains(&(event.owner as usize)) {
        return INVALID_ARGUMENT;
    }
    let Ok(mut state) = cmd_ref.state.lock() else {
        return PEER_LOST;
    };
    if state.kind != CommStateKind::Recording {
        return INVALID_ARGUMENT;
    }
    if state.event_references.insert(point.event as usize) {
        event.recordings.fetch_add(1, Ordering::Release);
    }
    state.prerequisites.push(point);
    SUCCESS
}

fn record_transfer(cmd: *mut CommCommand, kind: TransferKind, request: TransferRequest) -> i32 {
    let TransferRequest {
        local_rank,
        peer_rank,
        channel,
        sequence,
        address,
        bytes,
    } = request;
    if cmd.is_null()
        || bytes == 0
        || bytes > MAX_MESSAGE_BYTES
        || address.checked_add(bytes).is_none()
    {
        return INVALID_ARGUMENT;
    }
    let cmd_ref = unsafe { &*cmd };
    let endpoint = unsafe { &*cmd_ref.endpoint };
    let group = unsafe { &*endpoint.group };
    let cluster = unsafe { &*group.cluster };
    let Some(binding) = endpoint
        .bindings
        .iter()
        .find(|binding| binding.rank == local_rank)
    else {
        return INVALID_ARGUMENT;
    };
    let Some(peer) = group.members.get(peer_rank as usize) else {
        return INVALID_ARGUMENT;
    };
    let local_node =
        cluster.inventory[group.members[local_rank as usize].index as usize].node_index;
    let peer_node = cluster.inventory[peer.index as usize].node_index;
    if local_node == peer_node {
        return UNSUPPORTED;
    }
    let Ok(mut state) = cmd_ref.state.lock() else {
        return PEER_LOST;
    };
    if state.kind != CommStateKind::Recording {
        return INVALID_ARGUMENT;
    }
    let device = unsafe { &*(binding.device as *mut Device) };
    let Ok(mut allocations) = device.allocations.lock() else {
        return DEVICE_LOST;
    };
    let Some((&base, allocation)) = allocations.range(..=address).next_back() else {
        state.kind = CommStateKind::Invalid;
        return INVALID_ARGUMENT;
    };
    let access = if kind == TransferKind::Send { 1 } else { 2 };
    if !allocation.backend.contains(address, bytes) || allocation.access & access != access {
        state.kind = CommStateKind::Invalid;
        return INVALID_ARGUMENT;
    }
    let reference = (binding.device, base);
    if state.references.insert(reference) {
        allocations
            .get_mut(&base)
            .expect("validated allocation")
            .recordings += 1;
    }
    let source_rank = if kind == TransferKind::Send {
        local_rank
    } else {
        peer_rank
    };
    let destination_rank = if kind == TransferKind::Send {
        peer_rank
    } else {
        local_rank
    };
    state.operations.push(RecordedTransfer {
        kind,
        source_rank,
        destination_rank,
        channel,
        sequence,
        device: binding.device,
        queue: binding.queue,
        address,
        bytes,
    });
    SUCCESS
}

/// Records a point-to-point send from one bound local device rank.
///
/// # Safety
/// `cmd` must be live and `source` a valid retained device range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_send(
    cmd: *mut CommCommand,
    local_rank: u32,
    peer_rank: u32,
    channel: u64,
    sequence: u64,
    source: u64,
    bytes: u64,
) -> i32 {
    record_transfer(
        cmd,
        TransferKind::Send,
        TransferRequest {
            local_rank,
            peer_rank,
            channel,
            sequence: SequenceRef::Static(sequence),
            address: source,
            bytes,
        },
    )
}

/// Records a send whose sequence is bound at each submission.
///
/// # Safety
/// `cmd` must be live and `source` a retained device range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_send_slot(
    cmd: *mut CommCommand,
    local_rank: u32,
    peer_rank: u32,
    channel: u64,
    slot: u32,
    source: u64,
    bytes: u64,
) -> i32 {
    record_transfer(
        cmd,
        TransferKind::Send,
        TransferRequest {
            local_rank,
            peer_rank,
            channel,
            sequence: SequenceRef::Slot(slot),
            address: source,
            bytes,
        },
    )
}

/// Records a point-to-point receive into one bound local device rank.
///
/// # Safety
/// `cmd` must be live and `destination` a valid retained device range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_recv(
    cmd: *mut CommCommand,
    local_rank: u32,
    peer_rank: u32,
    channel: u64,
    sequence: u64,
    destination: u64,
    bytes: u64,
) -> i32 {
    record_transfer(
        cmd,
        TransferKind::Recv,
        TransferRequest {
            local_rank,
            peer_rank,
            channel,
            sequence: SequenceRef::Static(sequence),
            address: destination,
            bytes,
        },
    )
}

/// Records a receive whose sequence is bound at each submission.
///
/// # Safety
/// `cmd` must be live and `destination` a retained device range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_recv_slot(
    cmd: *mut CommCommand,
    local_rank: u32,
    peer_rank: u32,
    channel: u64,
    slot: u32,
    destination: u64,
    bytes: u64,
) -> i32 {
    record_transfer(
        cmd,
        TransferKind::Recv,
        TransferRequest {
            local_rank,
            peer_rank,
            channel,
            sequence: SequenceRef::Slot(slot),
            address: destination,
            bytes,
        },
    )
}

/// Seals a communication recording for submission.
///
/// # Safety
/// `cmd` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_end(cmd: *mut CommCommand) -> i32 {
    if cmd.is_null() {
        return INVALID_ARGUMENT;
    }
    let Ok(mut state) = (unsafe { &*cmd }).state.lock() else {
        return PEER_LOST;
    };
    if state.kind != CommStateKind::Recording || state.operations.is_empty() {
        return INVALID_ARGUMENT;
    }
    state.kind = CommStateKind::Executable;
    SUCCESS
}

fn reset_command(cmd: &CommCommand) -> i32 {
    let Ok(mut state) = cmd.state.lock() else {
        return PEER_LOST;
    };
    let endpoint = unsafe { &*cmd.endpoint };
    if state.kind == CommStateKind::Pending && endpoint.shared.failure.load(Ordering::Acquire) == 0
    {
        let point = state.pending.expect("pending communication has completion");
        let event = unsafe { &*point.event };
        match event.timeline.query() {
            Ok(value) if value >= point.value => {}
            Ok(_) => return BUSY,
            Err(_) => return DEVICE_LOST,
        }
    }
    for (device, base) in std::mem::take(&mut state.references) {
        let device = unsafe { &*(device as *mut Device) };
        let mut allocations = device
            .allocations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        allocations
            .get_mut(&base)
            .expect("retained communication allocation")
            .recordings -= 1;
    }
    for event in std::mem::take(&mut state.event_references) {
        unsafe {
            (*(event as *mut Event))
                .recordings
                .fetch_sub(1, Ordering::Release)
        };
    }
    state.operations.clear();
    state.prerequisites.clear();
    state.pending = None;
    state.kind = CommStateKind::Initial;
    SUCCESS
}

/// Resets an idle or failed communication recording.
///
/// # Safety
/// `cmd` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_reset(cmd: *mut CommCommand) -> i32 {
    if cmd.is_null() {
        return INVALID_ARGUMENT;
    }
    reset_command(unsafe { &*cmd })
}

/// Destroys an idle communication recording without a hidden wait.
///
/// # Safety
/// `cmd` must be a live handle from `nisyros_comm_cmd_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_comm_cmd_destroy(cmd: *mut CommCommand) -> i32 {
    if cmd.is_null() {
        return INVALID_ARGUMENT;
    }
    let cmd_ref = unsafe { &*cmd };
    let status = reset_command(cmd_ref);
    if status != SUCCESS {
        return status;
    }
    let endpoint = cmd_ref.endpoint;
    unsafe { drop(Box::from_raw(cmd)) };
    unsafe { (*endpoint).live_commands.fetch_sub(1, Ordering::Release) };
    SUCCESS
}

/// Submits one sealed communication recording to the software endpoint.
///
/// # Safety
/// `endpoint` and `cmd` must be live, and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nisyros_endpoint_submit(
    endpoint: *mut Endpoint,
    cmd: *mut CommCommand,
    desc: *const CommSubmitDesc,
    out: *mut Point,
) -> i32 {
    if endpoint.is_null() || cmd.is_null() || out.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        *out = Point {
            event: ptr::null_mut(),
            value: 0,
        }
    };
    let endpoint_ref = unsafe { &*endpoint };
    let cmd_ref = unsafe { &*cmd };
    if cmd_ref.endpoint != endpoint {
        return INVALID_ARGUMENT;
    }
    let failure = endpoint_ref.shared.failure.load(Ordering::Acquire);
    if failure != 0 {
        return failure;
    }
    let Ok(mut command) = cmd_ref.state.lock() else {
        return PEER_LOST;
    };
    if command.kind == CommStateKind::Pending {
        let point = command
            .pending
            .expect("pending communication has completion");
        let event = unsafe { &*point.event };
        if event
            .timeline
            .query()
            .is_ok_and(|value| value >= point.value)
        {
            command.kind = CommStateKind::Executable;
            command.pending = None;
        } else {
            return BUSY;
        }
    }
    if command.kind != CommStateKind::Executable {
        return INVALID_ARGUMENT;
    }
    let mut used_slots = BTreeSet::new();
    for operation in &command.operations {
        if let SequenceRef::Slot(slot) = operation.sequence {
            used_slots.insert(slot);
        }
    }
    let mut bindings = BTreeMap::new();
    if !desc.is_null() {
        let desc = unsafe { &*desc };
        let status = validate_header(&desc.header, std::mem::size_of::<CommSubmitDesc>());
        if status != SUCCESS {
            return status;
        }
        if desc.binding_count as usize != used_slots.len()
            || (desc.binding_count != 0 && desc.bindings.is_null())
        {
            return INVALID_ARGUMENT;
        }
        let slice = if desc.binding_count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(desc.bindings, desc.binding_count as usize) }
        };
        for binding in slice {
            if binding.reserved != 0
                || !used_slots.contains(&binding.slot)
                || bindings.insert(binding.slot, binding.sequence).is_some()
            {
                return INVALID_ARGUMENT;
            }
        }
    }
    if bindings.len() != used_slots.len() {
        return INVALID_ARGUMENT;
    }
    let group = unsafe { &*endpoint_ref.group };
    let operations: Vec<_> = command
        .operations
        .iter()
        .map(|recorded| {
            let sequence = match recorded.sequence {
                SequenceRef::Static(value) => value,
                SequenceRef::Slot(slot) => bindings[&slot],
            };
            TransferOp {
                kind: recorded.kind,
                key: MessageKey {
                    group_epoch: group.epoch,
                    source_rank: recorded.source_rank,
                    destination_rank: recorded.destination_rank,
                    channel: recorded.channel,
                    sequence,
                },
                device: recorded.device,
                queue: recorded.queue,
                address: recorded.address,
                bytes: recorded.bytes,
            }
        })
        .collect();
    if endpoint_ref.shared.pending.load(Ordering::Acquire) >= MAX_PENDING_MESSAGES {
        return BUSY;
    }
    let Ok(mut state) = endpoint_ref.shared.state.lock() else {
        return PEER_LOST;
    };
    let mut keys = BTreeSet::new();
    for operation in &operations {
        if !keys.insert(operation.key)
            || state.used_keys.contains(&operation.key)
            || state.active_keys.contains(&operation.key)
        {
            return INVALID_ARGUMENT;
        }
    }
    let Some(value) = state.next_value.checked_add(1) else {
        return INVALID_ARGUMENT;
    };
    state.next_value = value;
    state.used_keys.extend(&keys);
    state.active_keys.extend(keys);
    endpoint_ref.shared.pending.fetch_add(1, Ordering::Release);
    let point = Point {
        event: endpoint_ref.shared.completion as *mut Event,
        value,
    };
    command.kind = CommStateKind::Pending;
    command.pending = Some(point);
    let task = Task {
        value,
        operations,
        prerequisites: command
            .prerequisites
            .iter()
            .map(|point| (point.event as usize, point.value))
            .collect(),
    };
    if endpoint_ref
        .sender
        .send(ProgressMessage::Submit(task))
        .is_err()
    {
        mark_failed(&endpoint_ref.shared, PEER_LOST);
        command.kind = CommStateKind::Invalid;
        return PEER_LOST;
    }
    unsafe { *out = point };
    SUCCESS
}

struct PendingTransfer {
    operation: TransferOp,
    submission: u64,
    ready: bool,
}

struct ProgressState {
    group_epoch: u64,
    rank_count: u32,
    local_ranks: BTreeSet<u32>,
    sends: BTreeMap<MessageKey, PendingTransfer>,
    recvs: BTreeMap<MessageKey, PendingTransfer>,
    offers: BTreeMap<MessageKey, u64>,
    awaiting_ack: BTreeMap<MessageKey, PendingTransfer>,
    deferred: Vec<Task>,
    remaining: BTreeMap<u64, usize>,
    finished: BTreeSet<u64>,
    completed: u64,
}

impl ProgressState {
    fn new(group_epoch: u64, rank_count: u32, local_ranks: BTreeSet<u32>) -> Self {
        Self {
            group_epoch,
            rank_count,
            local_ranks,
            sends: BTreeMap::new(),
            recvs: BTreeMap::new(),
            offers: BTreeMap::new(),
            awaiting_ack: BTreeMap::new(),
            deferred: Vec::new(),
            remaining: BTreeMap::new(),
            finished: BTreeSet::new(),
            completed: 0,
        }
    }

    fn prerequisites_ready(task: &Task) -> Result<bool, i32> {
        for &(event_address, value) in &task.prerequisites {
            // SAFETY: The communication recording retains its prerequisite
            // events until this submission completes or fails.
            let event = unsafe { &*(event_address as *const Event) };
            let failure = event_failure(event);
            if failure != 0 {
                return Err(failure);
            }
            match event.timeline.query() {
                Ok(completed) if completed >= value => {}
                Ok(_) => return Ok(false),
                Err(_) => return Err(DEVICE_LOST),
            }
        }
        Ok(true)
    }

    fn enqueue(&mut self, task: Task, writer: &mut TcpStream) -> Result<(), i32> {
        if self.deferred.len() + self.remaining.len() >= MAX_PENDING_MESSAGES {
            return Err(BUSY);
        }
        if Self::prerequisites_ready(&task)? {
            self.submit(task, writer)
        } else {
            self.deferred.push(task);
            Ok(())
        }
    }

    fn poll_deferred(&mut self, writer: &mut TcpStream) -> Result<(), i32> {
        let mut index = 0;
        while index < self.deferred.len() {
            if Self::prerequisites_ready(&self.deferred[index])? {
                let task = self.deferred.remove(index);
                self.submit(task, writer)?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn submit(&mut self, task: Task, writer: &mut TcpStream) -> Result<(), i32> {
        if self.remaining.len() >= MAX_PENDING_MESSAGES {
            return Err(BUSY);
        }
        self.remaining.insert(task.value, task.operations.len());
        for operation in task.operations {
            let key = operation.key;
            let pending = PendingTransfer {
                operation,
                submission: task.value,
                ready: false,
            };
            if operation.kind == TransferKind::Send {
                if self.sends.insert(key, pending).is_some() {
                    return Err(PEER_LOST);
                }
                transport::write_frame(
                    writer,
                    &Frame {
                        kind: FrameKind::Offer,
                        key,
                        bytes: operation.bytes,
                        payload: Vec::new(),
                    },
                )
                .map_err(|_| PEER_LOST)?;
            } else {
                if self.recvs.insert(key, pending).is_some() {
                    return Err(PEER_LOST);
                }
                if let Some(offered_bytes) = self.offers.remove(&key) {
                    if offered_bytes != operation.bytes {
                        return Err(PEER_LOST);
                    }
                    self.recvs.get_mut(&key).expect("inserted receive").ready = true;
                    transport::write_frame(
                        writer,
                        &Frame {
                            kind: FrameKind::Ready,
                            key,
                            bytes: operation.bytes,
                            payload: Vec::new(),
                        },
                    )
                    .map_err(|_| PEER_LOST)?;
                }
            }
        }
        Ok(())
    }

    fn remote(&mut self, frame: Frame, writer: &mut TcpStream, shared: &Shared) -> Result<(), i32> {
        let key = frame.key;
        if frame.kind != FrameKind::Heartbeat {
            let source_local = self.local_ranks.contains(&key.source_rank);
            let destination_local = self.local_ranks.contains(&key.destination_rank);
            if key.group_epoch != self.group_epoch
                || key.source_rank >= self.rank_count
                || key.destination_rank >= self.rank_count
                || match frame.kind {
                    FrameKind::Offer | FrameKind::Data => source_local || !destination_local,
                    FrameKind::Ready | FrameKind::Ack => !source_local || destination_local,
                    FrameKind::Heartbeat => false,
                }
            {
                return Err(PEER_LOST);
            }
        }
        match frame.kind {
            FrameKind::Heartbeat => return Ok(()),
            FrameKind::Offer => {
                if let Some(receive) = self.recvs.get_mut(&key) {
                    if receive.ready || receive.operation.bytes != frame.bytes {
                        return Err(PEER_LOST);
                    }
                    receive.ready = true;
                    transport::write_frame(
                        writer,
                        &Frame {
                            kind: FrameKind::Ready,
                            key,
                            bytes: frame.bytes,
                            payload: Vec::new(),
                        },
                    )
                    .map_err(|_| PEER_LOST)?;
                } else if self.offers.insert(key, frame.bytes).is_some()
                    || self.offers.len() > MAX_PENDING_MESSAGES
                {
                    return Err(PEER_LOST);
                }
            }
            FrameKind::Ready => {
                let Some(send) = self.sends.remove(&key) else {
                    return Err(PEER_LOST);
                };
                if send.operation.bytes != frame.bytes {
                    return Err(PEER_LOST);
                }
                let data = gpu_copy(&send.operation, None)?;
                transport::write_frame(
                    writer,
                    &Frame {
                        kind: FrameKind::Data,
                        key,
                        bytes: frame.bytes,
                        payload: data,
                    },
                )
                .map_err(|_| PEER_LOST)?;
                self.awaiting_ack.insert(key, send);
            }
            FrameKind::Data => {
                let Some(receive) = self.recvs.remove(&key) else {
                    return Err(PEER_LOST);
                };
                if !receive.ready || receive.operation.bytes != frame.bytes {
                    return Err(PEER_LOST);
                }
                gpu_copy(&receive.operation, Some(&frame.payload))?;
                transport::write_frame(
                    writer,
                    &Frame {
                        kind: FrameKind::Ack,
                        key,
                        bytes: frame.bytes,
                        payload: Vec::new(),
                    },
                )
                .map_err(|_| PEER_LOST)?;
                self.complete(receive, shared)?;
            }
            FrameKind::Ack => {
                let Some(send) = self.awaiting_ack.remove(&key) else {
                    return Err(PEER_LOST);
                };
                if send.operation.bytes != frame.bytes {
                    return Err(PEER_LOST);
                }
                self.complete(send, shared)?;
            }
        }
        Ok(())
    }

    fn complete(&mut self, transfer: PendingTransfer, shared: &Shared) -> Result<(), i32> {
        let Some(remaining) = self.remaining.get_mut(&transfer.submission) else {
            return Err(PEER_LOST);
        };
        *remaining -= 1;
        shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active_keys
            .remove(&transfer.operation.key);
        if *remaining == 0 {
            self.remaining.remove(&transfer.submission);
            self.finished.insert(transfer.submission);
        }
        while self.finished.remove(&(self.completed + 1)) {
            let next = self.completed + 1;
            // SAFETY: The endpoint retains its host completion event until
            // all accepted submissions have finished or failed.
            let status =
                unsafe { nisyros_event_host_signal(shared.completion as *mut Event, next) };
            if status != SUCCESS {
                return Err(status);
            }
            self.completed = next;
            shared.pending.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }
}

fn mark_failed(shared: &Shared, status: i32) {
    if shared
        .failure
        .compare_exchange(0, status, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        // SAFETY: The endpoint retains this event until its worker exits.
        unsafe {
            (*(shared.completion as *mut Event))
                .failure
                .store(status, Ordering::Release)
        };
    }
}

fn progress_loop(
    mut writer: TcpStream,
    receiver: mpsc::Receiver<ProgressMessage>,
    sender: mpsc::Sender<ProgressMessage>,
    shared: Arc<Shared>,
    config: ProgressConfig,
) {
    let ProgressConfig {
        bindings,
        group_epoch,
        rank_count,
        timeout,
    } = config;
    let Ok(mut reader_stream) = writer.try_clone() else {
        mark_failed(&shared, PEER_LOST);
        return;
    };
    if reader_stream.set_read_timeout(Some(timeout)).is_err()
        || writer.set_write_timeout(Some(timeout)).is_err()
    {
        mark_failed(&shared, PEER_LOST);
        return;
    }
    let reader = match thread::Builder::new()
        .name("nisyros-reader".into())
        .spawn(move || {
            loop {
                match transport::read_frame(&mut reader_stream) {
                    Ok(frame) => {
                        if sender.send(ProgressMessage::Remote(frame)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(ProgressMessage::PeerFailed);
                        break;
                    }
                }
            }
        }) {
        Ok(reader) => reader,
        Err(_) => {
            mark_failed(&shared, PEER_LOST);
            return;
        }
    };
    let local_ranks = bindings.iter().map(|binding| binding.rank).collect();
    let mut progress = ProgressState::new(group_epoch, rank_count, local_ranks);
    let heartbeat = (timeout / 4).min(Duration::from_secs(1));
    let mut last_heartbeat = Instant::now();
    loop {
        let wait = if progress.deferred.is_empty() {
            heartbeat
        } else {
            Duration::from_millis(2)
        };
        let result = match receiver.recv_timeout(wait) {
            Ok(ProgressMessage::Submit(task)) => progress.enqueue(task, &mut writer),
            Ok(ProgressMessage::Remote(frame)) => progress.remote(frame, &mut writer, &shared),
            Ok(ProgressMessage::PeerFailed) => Err(PEER_LOST),
            Ok(ProgressMessage::Shutdown) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if last_heartbeat.elapsed() >= heartbeat => {
                last_heartbeat = Instant::now();
                transport::write_frame(&mut writer, &Frame::heartbeat()).map_err(|_| PEER_LOST)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Err(status) = result {
            mark_failed(&shared, status);
            break;
        }
        if let Err(status) = progress.poll_deferred(&mut writer) {
            mark_failed(&shared, status);
            break;
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
    let _ = reader.join();
}

fn gpu_copy(operation: &TransferOp, incoming: Option<&[u8]>) -> Result<Vec<u8>, i32> {
    let device = operation.device as *mut Device;
    let queue = operation.queue as *mut Queue;
    let host_desc = HostAllocDesc {
        header: DescriptorHeader {
            struct_size: std::mem::size_of::<HostAllocDesc>() as u32,
            abi_version: ABI_VERSION,
            extensions: ptr::null(),
        },
        bytes: operation.bytes,
        alignment: 64,
        reserved: [0; 2],
    };
    let mut host = ptr::null_mut();
    // SAFETY: Endpoint bindings retain the device and queue, and the command
    // retains the allocation range for this transfer.
    let status = unsafe { nisyros_host_alloc(device, &host_desc, &mut host) };
    if status != SUCCESS {
        return Err(status);
    }
    if let Some(data) = incoming {
        if data.len() as u64 != operation.bytes {
            unsafe { nisyros_host_free(device, host) };
            return Err(PEER_LOST);
        }
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), host.cast::<u8>(), data.len()) };
    }
    let mut command = ptr::null_mut();
    let mut result = unsafe { nisyros_cmd_create(device, (*queue).family, &mut command) };
    if result == SUCCESS {
        result = unsafe { nisyros_cmd_begin(command) };
    }
    if result == SUCCESS {
        result = unsafe {
            if incoming.is_some() {
                nisyros_cmd_copy_h2d(command, operation.address, host, operation.bytes)
            } else {
                nisyros_cmd_copy_d2h(command, host, operation.address, operation.bytes)
            }
        };
    }
    if result == SUCCESS {
        result = unsafe { nisyros_cmd_end(command) };
    }
    let mut done = Point {
        event: ptr::null_mut(),
        value: 0,
    };
    if result == SUCCESS {
        result = unsafe { nisyros_submit(queue, command, ptr::null(), &mut done) };
    }
    if result == SUCCESS {
        result = unsafe { nisyros_wait(done, u64::MAX) };
    }
    let output = if result == SUCCESS && incoming.is_none() {
        unsafe { std::slice::from_raw_parts(host.cast::<u8>(), operation.bytes as usize) }.to_vec()
    } else {
        Vec::new()
    };
    if !command.is_null() {
        let destroyed = unsafe { nisyros_cmd_destroy(command) };
        if result == SUCCESS {
            result = destroyed;
        }
    }
    let freed = unsafe { nisyros_host_free(device, host) };
    if result == SUCCESS {
        result = freed;
    }
    if result == SUCCESS {
        Ok(output)
    } else {
        Err(result)
    }
}

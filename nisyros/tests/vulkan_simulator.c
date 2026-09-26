// Test-only Vulkan loader replacement. It models the calls Nisyros uses for
// discovery, host-visible memory, address copies, and timeline submissions.
#define _POSIX_C_SOURCE 200809L
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct { int32_t type; void *next; } header;
typedef struct { uint64_t address, size; } range;
typedef struct { int32_t type; const void *next; uint32_t flags; uint64_t size; uint32_t usage, sharing, family_count; const uint32_t *families; } buffer_info;
typedef struct { uint64_t size, alignment; uint32_t types; } requirements;
typedef struct { int32_t type; const void *next; uint64_t size; uint32_t memory_type; } memory_info;
typedef struct { int32_t type; const void *next; uint64_t buffer; } address_info;
typedef struct { int32_t type; const void *next; range source; uint32_t source_flags; range destination; uint32_t destination_flags; } copy_region;
typedef struct { int32_t type; const void *next; uint32_t count; const copy_region *regions; } copy_info;
typedef struct { int32_t type; const void *next; uint64_t semaphore, value, stage; uint32_t device_index; } semaphore_submit;
typedef struct { int32_t type; const void *next; void *command; uint32_t device_mask; } command_submit;
typedef struct { int32_t type; const void *next; uint32_t flags, wait_count; const semaphore_submit *waits; uint32_t command_count; const command_submit *commands; uint32_t signal_count; const semaphore_submit *signals; } submit_info;
typedef struct { int32_t type; const void *next; uint32_t flags, count; const uint64_t *semaphores, *values; } wait_info;
typedef struct { int32_t type; const void *next; uint64_t semaphore, value; } signal_info;
typedef struct { int32_t type; const void *next; int32_t semaphore_type; uint64_t initial; } semaphore_type_info;
typedef struct { uint32_t flags, count, timestamp_bits, granularity[3]; } queue_family;
typedef struct { char name[256]; uint32_t version; } extension;
typedef struct { uint32_t flags, heap; } memory_type;
typedef struct { uint64_t size; uint32_t flags, pad; } memory_heap;
typedef struct { uint32_t count; memory_type types[32]; uint32_t heap_count; memory_heap heaps[16]; } memory_properties;
typedef struct { int32_t type; void *next; uint8_t uuid[16], driver_uuid[16], luid[8]; uint32_t node_mask, valid; } id_properties;
typedef struct { int32_t type; void *next; int32_t driver_id; char name[256], info[256]; uint8_t conformance[4]; } driver_properties;
typedef struct { uint32_t api, driver, vendor, device, device_type; char name[256]; uint8_t cache_uuid[16]; uint32_t pad; uint8_t before_grid[220]; uint32_t grid[3]; } properties_prefix;

typedef struct { size_t size; unsigned char *data; } memory;
typedef struct { size_t size; memory *bound; uint64_t offset; } buffer;
typedef struct { uint64_t value; } semaphore;
typedef struct { int kind; uint64_t dst, src, bytes; uint32_t pattern; } operation;
typedef struct { operation ops[256]; size_t count; } command;
typedef struct pending { struct pending *next; command *cmd; uint32_t wait_count, signal_count; semaphore_submit waits[16], signals[16]; } pending;
typedef struct { pending *head, *tail; } queue;
typedef struct { queue queues[2]; } device;

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static device *devices[16];
static size_t device_count;

static int physical_count(void) {
    const char *value = getenv("NISYROS_SIM_DEVICES");
    int count = value ? atoi(value) : 1;
    return count >= 1 && count <= 2 ? count : 1;
}

static void *physical_handle(int index) { return (void *)(uintptr_t)(index + 1); }
static int physical_index(void *handle) { return (int)(uintptr_t)handle - 1; }
static void *symbol(const char *name) { return dlsym(RTLD_DEFAULT, name); }

void *vkGetInstanceProcAddr(void *instance, const char *name) { (void)instance; return symbol(name); }
void *vkGetDeviceProcAddr(void *handle, const char *name) { (void)handle; return symbol(name); }
int vkCreateInstance(const void *info, const void *allocator, void **out) {
    (void)info; (void)allocator; *out = physical_handle(16); return 0;
}
void vkDestroyInstance(void *instance, const void *allocator) { (void)instance; (void)allocator; }
int vkEnumeratePhysicalDevices(void *instance, uint32_t *count, void **out) {
    (void)instance;
    uint32_t available = (uint32_t)physical_count();
    if (out) {
        uint32_t written = *count < available ? *count : available;
        for (uint32_t i = 0; i < written; ++i) out[i] = physical_handle((int)i);
        *count = written;
        return written < available ? 5 : 0;
    }
    *count = available;
    return 0;
}
int vkEnumerateDeviceExtensionProperties(void *physical, const char *layer, uint32_t *count, extension *out) {
    (void)physical; (void)layer;
    static const char *names[] = {
        "VK_KHR_pipeline_binary", "VK_KHR_device_address_commands", "VK_KHR_maintenance5"
    };
    if (!out) { *count = 3; return 0; }
    uint32_t written = *count < 3 ? *count : 3;
    for (uint32_t i = 0; i < written; ++i) {
        memset(&out[i], 0, sizeof(out[i]));
        strcpy(out[i].name, names[i]);
        out[i].version = 1;
    }
    *count = written;
    return written == 3 ? 0 : 5;
}
void vkGetPhysicalDeviceProperties(void *physical, properties_prefix *out) {
    int index = physical_index(physical);
    memset(out, 0, sizeof(*out));
    out->api = (1u << 22) | (3u << 12);
    out->driver = 1;
    out->vendor = 0xCAFE;
    out->device = (uint32_t)(0x5000 + index);
    out->device_type = 2;
    strcpy(out->name, "Nisyros test Vulkan simulator");
    out->cache_uuid[0] = 0xA5;
    out->cache_uuid[1] = (uint8_t)index;
    out->grid[0] = out->grid[1] = out->grid[2] = 65535;
}
void vkGetPhysicalDeviceProperties2(void *physical, void *raw) {
    header *head = raw;
    vkGetPhysicalDeviceProperties(physical, (properties_prefix *)((unsigned char *)raw + sizeof(header)));
    for (header *part = head->next; part; part = part->next) {
        if (part->type == 1000071004) {
            id_properties *id = (id_properties *)part;
            id->uuid[0] = 0xB6;
            id->uuid[1] = (uint8_t)physical_index(physical);
            id->driver_uuid[0] = 0xC7;
        } else if (part->type == 1000196000) {
            driver_properties *driver = (driver_properties *)part;
            driver->driver_id = 1;
            strcpy(driver->name, "Nisyros simulator");
        }
    }
}
void vkGetPhysicalDeviceFeatures2(void *physical, void *raw) {
    (void)physical;
    for (header *part = ((header *)raw)->next; part; part = part->next) {
        switch (part->type) {
        case 1000207000: case 1000257000: case 1000314007:
        case 1000470000: case 1000318006: case 1000483000:
            *(uint32_t *)((unsigned char *)part + sizeof(header)) = 1;
            break;
        }
    }
}
void vkGetPhysicalDeviceQueueFamilyProperties(void *physical, uint32_t *count, queue_family *out) {
    (void)physical;
    if (!out) { *count = 1; return; }
    if (*count) { out[0] = (queue_family){.flags = 7, .count = 2, .granularity = {1, 1, 1}}; *count = 1; }
}
void vkGetPhysicalDeviceMemoryProperties(void *physical, memory_properties *out) {
    (void)physical;
    memset(out, 0, sizeof(*out));
    out->count = 1;
    out->types[0] = (memory_type){7, 0};
    out->heap_count = 1;
    out->heaps[0] = (memory_heap){UINT64_C(1) << 34, 1, 0};
}
void vkGetPhysicalDeviceExternalBufferProperties(void *physical, const void *info, void *out) {
    (void)physical; (void)info; (void)out;
}
void vkGetPhysicalDeviceExternalSemaphoreProperties(void *physical, const void *info, void *out) {
    (void)physical; (void)info; (void)out;
}
int vkCreateDevice(void *physical, const void *info, const void *allocator, void **out) {
    (void)physical; (void)info; (void)allocator;
    device *created = calloc(1, sizeof(*created));
    if (!created) return -1;
    pthread_mutex_lock(&lock);
    if (device_count < 16) devices[device_count++] = created;
    pthread_mutex_unlock(&lock);
    *out = created;
    return 0;
}
void vkDestroyDevice(void *handle, const void *allocator) {
    (void)allocator;
    pthread_mutex_lock(&lock);
    for (size_t i = 0; i < device_count; ++i) if (devices[i] == handle) devices[i] = NULL;
    pthread_mutex_unlock(&lock);
    free(handle);
}
void vkGetDeviceQueue(void *handle, uint32_t family, uint32_t index, void **out) {
    (void)family;
    *out = index < 2 ? &((device *)handle)->queues[index] : NULL;
}

int vkCreateBuffer(void *handle, const buffer_info *info, const void *allocator, uint64_t *out) {
    (void)handle; (void)allocator;
    buffer *created = calloc(1, sizeof(*created));
    if (!created) return -1;
    created->size = (size_t)info->size;
    *out = (uint64_t)(uintptr_t)created;
    return 0;
}
void vkDestroyBuffer(void *handle, uint64_t raw, const void *allocator) {
    (void)handle; (void)allocator; free((void *)(uintptr_t)raw);
}
void vkGetBufferMemoryRequirements(void *handle, uint64_t raw, requirements *out) {
    (void)handle;
    *out = (requirements){((buffer *)(uintptr_t)raw)->size, 64, 1};
}
int vkAllocateMemory(void *handle, const memory_info *info, const void *allocator, uint64_t *out) {
    (void)handle; (void)allocator;
    memory *created = calloc(1, sizeof(*created));
    if (!created) return -1;
    created->size = (size_t)info->size;
    if (posix_memalign((void **)&created->data, 4096, created->size) != 0) { free(created); return -1; }
    memset(created->data, 0, created->size);
    *out = (uint64_t)(uintptr_t)created;
    return 0;
}
void vkFreeMemory(void *handle, uint64_t raw, const void *allocator) {
    (void)handle; (void)allocator;
    memory *allocation = (memory *)(uintptr_t)raw;
    free(allocation->data); free(allocation);
}
int vkBindBufferMemory(void *handle, uint64_t raw_buffer, uint64_t raw_memory, uint64_t offset) {
    (void)handle;
    buffer *bound = (buffer *)(uintptr_t)raw_buffer;
    bound->bound = (memory *)(uintptr_t)raw_memory;
    bound->offset = offset;
    return 0;
}
uint64_t vkGetBufferDeviceAddress(void *handle, const address_info *info) {
    (void)handle;
    buffer *bound = (buffer *)(uintptr_t)info->buffer;
    return (uint64_t)(uintptr_t)(bound->bound->data + bound->offset);
}
int vkMapMemory(void *handle, uint64_t raw, uint64_t offset, uint64_t size, uint32_t flags, void **out) {
    (void)handle; (void)size; (void)flags;
    *out = ((memory *)(uintptr_t)raw)->data + offset;
    return 0;
}
void vkUnmapMemory(void *handle, uint64_t raw) { (void)handle; (void)raw; }
int vkFlushMappedMemoryRanges(void *handle, uint32_t count, const void *ranges) {
    (void)handle; (void)count; (void)ranges; return 0;
}
int vkInvalidateMappedMemoryRanges(void *handle, uint32_t count, const void *ranges) {
    (void)handle; (void)count; (void)ranges; return 0;
}

int vkCreateCommandPool(void *handle, const void *info, const void *allocator, uint64_t *out) {
    (void)handle; (void)info; (void)allocator;
    *out = (uint64_t)(uintptr_t)calloc(1, sizeof(command));
    return *out ? 0 : -1;
}
void vkDestroyCommandPool(void *handle, uint64_t pool, const void *allocator) {
    (void)handle; (void)allocator; free((void *)(uintptr_t)pool);
}
int vkAllocateCommandBuffers(void *handle, const void *info, void **out) {
    (void)handle;
    // VkCommandBufferAllocateInfo places VkCommandPool after its 16-byte header.
    *out = (void *)(uintptr_t)*(const uint64_t *)((const unsigned char *)info + 16);
    return 0;
}
int vkBeginCommandBuffer(void *raw, const void *info) {
    (void)info;
    ((command *)raw)->count = 0;
    return 0;
}
int vkEndCommandBuffer(void *raw) { (void)raw; return 0; }
static void record(command *cmd, operation op) { if (cmd->count < 256) cmd->ops[cmd->count++] = op; }
void vkCmdCopyMemoryKHR(void *raw, const copy_info *info) {
    for (uint32_t i = 0; i < info->count; ++i) {
        const copy_region *region = &info->regions[i];
        record(raw, (operation){.kind = 1, .dst = region->destination.address,
                                .src = region->source.address, .bytes = region->source.size});
    }
}
void vkCmdFillMemoryKHR(void *raw, const range *target, uint32_t flags, uint32_t pattern) {
    (void)flags;
    record(raw, (operation){.kind = 2, .dst = target->address, .bytes = target->size, .pattern = pattern});
}
void vkCmdPipelineBarrier2(void *raw, const void *info) { (void)raw; (void)info; }
void vkCmdBindPipeline(void *raw, int point, uint64_t pipeline) { (void)raw; (void)point; (void)pipeline; }
void vkCmdPushConstants(void *raw, uint64_t layout, uint32_t stages, uint32_t offset, uint32_t size, const void *data) {
    (void)raw; (void)layout; (void)stages; (void)offset; (void)size; (void)data;
}
void vkCmdDispatch(void *raw, uint32_t x, uint32_t y, uint32_t z) { (void)raw; (void)x; (void)y; (void)z; }
void vkCmdDispatchIndirect2KHR(void *raw, const void *info) { (void)raw; (void)info; }

int vkCreateSemaphore(void *handle, const void *raw_info, const void *allocator, uint64_t *out) {
    (void)handle; (void)allocator;
    const header *info = raw_info;
    uint64_t initial = 0;
    if (info->next && ((const header *)info->next)->type == 1000207002)
        initial = ((const semaphore_type_info *)info->next)->initial;
    semaphore *created = calloc(1, sizeof(*created));
    if (!created) return -1;
    created->value = initial;
    *out = (uint64_t)(uintptr_t)created;
    return 0;
}
void vkDestroySemaphore(void *handle, uint64_t raw, const void *allocator) {
    (void)handle; (void)allocator; free((void *)(uintptr_t)raw);
}
static int ready(const pending *submission) {
    for (uint32_t i = 0; i < submission->wait_count; ++i)
        if (((semaphore *)(uintptr_t)submission->waits[i].semaphore)->value < submission->waits[i].value) return 0;
    return 1;
}
static void execute(const command *cmd) {
    for (size_t i = 0; i < cmd->count; ++i) {
        const operation *op = &cmd->ops[i];
        if (op->kind == 1) memmove((void *)(uintptr_t)op->dst, (const void *)(uintptr_t)op->src, (size_t)op->bytes);
        else if (op->kind == 2) {
            uint32_t *words = (uint32_t *)(uintptr_t)op->dst;
            for (uint64_t j = 0; j < op->bytes / 4; ++j) words[j] = op->pattern;
        }
    }
}
static void drain(void) {
    int progressed;
    do {
        progressed = 0;
        for (size_t d = 0; d < device_count; ++d) if (devices[d]) {
            for (int q = 0; q < 2; ++q) {
                queue *endpoint = &devices[d]->queues[q];
                pending *head = endpoint->head;
                if (!head || !ready(head)) continue;
                execute(head->cmd);
                for (uint32_t i = 0; i < head->signal_count; ++i) {
                    semaphore *target = (semaphore *)(uintptr_t)head->signals[i].semaphore;
                    if (target->value < head->signals[i].value) target->value = head->signals[i].value;
                }
                endpoint->head = head->next;
                if (!endpoint->head) endpoint->tail = NULL;
                free(head);
                progressed = 1;
                pthread_cond_broadcast(&changed);
            }
        }
    } while (progressed);
}
int vkQueueSubmit2(void *raw_queue, uint32_t count, const submit_info *submits, uint64_t fence) {
    (void)fence;
    queue *endpoint = raw_queue;
    pthread_mutex_lock(&lock);
    for (uint32_t i = 0; i < count; ++i) {
        const submit_info *src = &submits[i];
        if (src->wait_count > 16 || src->signal_count > 16 || src->command_count != 1) {
            pthread_mutex_unlock(&lock); return -3;
        }
        pending *item = calloc(1, sizeof(*item));
        if (!item) { pthread_mutex_unlock(&lock); return -1; }
        item->cmd = src->commands[0].command;
        item->wait_count = src->wait_count;
        item->signal_count = src->signal_count;
        memcpy(item->waits, src->waits, src->wait_count * sizeof(semaphore_submit));
        memcpy(item->signals, src->signals, src->signal_count * sizeof(semaphore_submit));
        if (endpoint->tail) endpoint->tail->next = item; else endpoint->head = item;
        endpoint->tail = item;
    }
    drain();
    pthread_mutex_unlock(&lock);
    return 0;
}
int vkGetSemaphoreCounterValue(void *handle, uint64_t raw, uint64_t *out) {
    (void)handle;
    pthread_mutex_lock(&lock);
    drain();
    *out = ((semaphore *)(uintptr_t)raw)->value;
    pthread_mutex_unlock(&lock);
    return 0;
}
int vkSignalSemaphore(void *handle, const signal_info *info) {
    (void)handle;
    pthread_mutex_lock(&lock);
    semaphore *target = (semaphore *)(uintptr_t)info->semaphore;
    if (target->value < info->value) target->value = info->value;
    drain();
    pthread_cond_broadcast(&changed);
    pthread_mutex_unlock(&lock);
    return 0;
}
int vkWaitSemaphores(void *handle, const wait_info *info, uint64_t timeout) {
    (void)handle;
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    if (timeout != UINT64_MAX) {
        deadline.tv_sec += (time_t)(timeout / 1000000000);
        deadline.tv_nsec += (long)(timeout % 1000000000);
        if (deadline.tv_nsec >= 1000000000) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000; }
    }
    pthread_mutex_lock(&lock);
    for (;;) {
        drain();
        int complete = 1;
        for (uint32_t i = 0; i < info->count; ++i)
            if (((semaphore *)(uintptr_t)info->semaphores[i])->value < info->values[i]) complete = 0;
        if (complete) { pthread_mutex_unlock(&lock); return 0; }
        if (timeout == 0) { pthread_mutex_unlock(&lock); return 2; }
        int status = timeout == UINT64_MAX ? pthread_cond_wait(&changed, &lock)
                                           : pthread_cond_timedwait(&changed, &lock, &deadline);
        if (status != 0) { pthread_mutex_unlock(&lock); return 2; }
    }
}

// Required by Device::open, but this fixture never creates a pipeline.
int vkCreateShaderModule(void *a, const void *b, const void *c, uint64_t *d) { (void)a; (void)b; (void)c; (void)d; return -8; }
void vkDestroyShaderModule(void *a, uint64_t b, const void *c) { (void)a; (void)b; (void)c; }
int vkCreatePipelineLayout(void *a, const void *b, const void *c, uint64_t *d) { (void)a; (void)b; (void)c; (void)d; return -8; }
void vkDestroyPipelineLayout(void *a, uint64_t b, const void *c) { (void)a; (void)b; (void)c; }
int vkCreateComputePipelines(void *a, uint64_t b, uint32_t c, const void *d, const void *e, uint64_t *f) {
    (void)a; (void)b; (void)c; (void)d; (void)e; (void)f; return -8;
}
void vkDestroyPipeline(void *a, uint64_t b, const void *c) { (void)a; (void)b; (void)c; }
int vkGetPipelineKeyKHR(void *a, const void *b, void *c) { (void)a; (void)b; (void)c; return -8; }
int vkCreatePipelineBinariesKHR(void *a, const void *b, const void *c, void *d) { (void)a; (void)b; (void)c; (void)d; return -8; }
void vkDestroyPipelineBinaryKHR(void *a, uint64_t b, const void *c) { (void)a; (void)b; (void)c; }
int vkGetPipelineBinaryDataKHR(void *a, const void *b, void *c, size_t *d, void *e) {
    (void)a; (void)b; (void)c; (void)d; (void)e; return -8;
}
int vkReleaseCapturedPipelineDataKHR(void *a, const void *b, const void *c) { (void)a; (void)b; (void)c; return -8; }

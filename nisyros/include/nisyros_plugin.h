#ifndef NISYROS_PLUGIN_H
#define NISYROS_PLUGIN_H

#include "nisyros.h"

#ifdef __cplusplus
extern "C" {
#endif

#define NISYROS_PLUGIN_ABI_VERSION 1u
#define NISYROS_PLUGIN_SERVICES_VERSION 1u

typedef struct {
    uint32_t struct_size;
    uint32_t abi_version;
    void *context;
    void (NISYROS_CALL *report_error)(void *context, nisyros_status status,
                                      int32_t backend_code, const char *message,
                                      uint32_t message_bytes);
} nisyros_plugin_services;

typedef struct {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t backend;
    uint32_t api_version;
    uint32_t vendor_id;
    uint32_t device_id;
    uint32_t device_type;
    uint32_t driver_id;
    uint32_t strict_profile_supported;
    uint32_t queue_family_count;
    uint32_t memory_heap_count;
    uint32_t cooperative_matrix_count;
    uint64_t features;
    uint64_t external_host_alignment;
    uint32_t cooperative_matrix_revision;
    uint32_t shader_bfloat16_revision;
    uint32_t shader_enqueue_revision;
    uint32_t cooperative_matrix_stages;
    uint32_t max_grid[3];
    uint32_t peer_memory_fd;
    uint32_t peer_timeline_fd;
    float timestamp_period_ns;
    char name[256];
    uint8_t device_uuid[16];
    uint8_t driver_uuid[16];
    char driver_name[256];
} nisyros_plugin_device_info;

typedef struct {
    uint32_t flags;
    uint32_t count;
    uint32_t timestamp_valid_bits;
} nisyros_plugin_queue_family_info;

typedef struct {
    uint64_t bytes;
    uint32_t flags;
    uint32_t reserved;
} nisyros_plugin_memory_heap_info;

typedef struct {
    uint32_t family;
    uint32_t count;
    float priority;
} nisyros_plugin_queue_request;

typedef struct {
    uint32_t struct_size;
    uint32_t abi_version;
    nisyros_status (NISYROS_CALL *instance_open)(
        const nisyros_plugin_services *services, void **out);
    void (NISYROS_CALL *instance_close)(void *instance);
    nisyros_status (NISYROS_CALL *device_count)(void *instance, uint32_t *out);
    nisyros_status (NISYROS_CALL *device_info)(
        void *instance, uint32_t index, nisyros_plugin_device_info *out);
    nisyros_status (NISYROS_CALL *queue_family_info)(
        void *instance, uint32_t device_index, uint32_t family,
        nisyros_plugin_queue_family_info *out);
    nisyros_status (NISYROS_CALL *memory_heap_info)(
        void *instance, uint32_t device_index, uint32_t heap,
        nisyros_plugin_memory_heap_info *out);
    nisyros_status (NISYROS_CALL *cooperative_matrix)(
        void *instance, uint32_t device_index, uint32_t tuple,
        nisyros_cooperative_matrix_tuple *out);
    nisyros_status (NISYROS_CALL *device_open)(
        void *instance, uint32_t index, uint32_t request_count,
        const nisyros_plugin_queue_request *requests, void **out);
    void (NISYROS_CALL *device_close)(void *device);
} nisyros_plugin_api;

/* All handles and callback storage belong to the provider that returned them. */
NISYROS_API nisyros_status NISYROS_CALL
nisyros_plugin_get_api(uint32_t abi_version,
                       const nisyros_plugin_services *services,
                       uint32_t table_size, void *table);

#ifdef __cplusplus
}
#endif

#endif

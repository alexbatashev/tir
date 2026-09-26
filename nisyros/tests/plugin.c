#include "nisyros_plugin.h"
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static void report_error(void *context, nisyros_status status, int32_t code,
                         const char *message, uint32_t message_bytes) {
    (void)status;
    (void)code;
    (void)message;
    (void)message_bytes;
    ++*(int *)context;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    assert(strcmp(argv[1], "strict") == 0 || strcmp(argv[1], "unsupported") == 0);
    int errors = 0;
    nisyros_plugin_services services = {
        sizeof(services), NISYROS_PLUGIN_SERVICES_VERSION, &errors, report_error,
    };
    nisyros_plugin_api api = {0};
    assert(nisyros_plugin_get_api(NISYROS_PLUGIN_ABI_VERSION, &services, 8,
                                  &api) == NISYROS_INCOMPLETE);
    assert(api.struct_size == sizeof(api) && api.instance_open == NULL);
    unsigned char oversized[sizeof(api) + 8];
    memset(oversized, 0xa5, sizeof(oversized));
    assert(nisyros_plugin_get_api(NISYROS_PLUGIN_ABI_VERSION, &services,
                                  sizeof(oversized), oversized) == NISYROS_SUCCESS);
    for (size_t i = sizeof(api); i < sizeof(oversized); ++i) assert(oversized[i] == 0xa5);
    assert(nisyros_plugin_get_api(NISYROS_PLUGIN_ABI_VERSION, &services,
                                  sizeof(api), &api) == NISYROS_SUCCESS);
    assert(api.instance_open && api.instance_close && api.device_count && api.device_info);
    assert(api.queue_family_info && api.memory_heap_info && api.cooperative_matrix);
    assert(api.device_open && api.device_close);
    nisyros_plugin_api unsupported = api;
    assert(nisyros_plugin_get_api(999, &services, sizeof(unsupported),
                                  &unsupported) == NISYROS_UNSUPPORTED);
    assert(unsupported.instance_open == NULL && unsupported.device_open == NULL);
    services.abi_version = 999;
    assert(nisyros_plugin_get_api(NISYROS_PLUGIN_ABI_VERSION, &services,
                                  sizeof(unsupported), &unsupported) == NISYROS_INVALID_ARGUMENT);
    assert(unsupported.instance_open == NULL);
    services.abi_version = NISYROS_PLUGIN_SERVICES_VERSION;

    void *instance = NULL;
    assert(api.instance_open(&services, &instance) == NISYROS_SUCCESS);
    assert(instance != NULL);
    uint32_t count = 0;
    assert(api.device_count(instance, &count) == NISYROS_SUCCESS);
    assert(count > 0 && count <= 32);
    nisyros_plugin_device_info info = {
        .struct_size = sizeof(info),
        .abi_version = NISYROS_PLUGIN_ABI_VERSION,
    };
    assert(api.device_info(instance, 0, &info) == NISYROS_SUCCESS);
    assert(info.backend == NISYROS_BACKEND_VULKAN);
    assert(info.name[0] && info.driver_name[0]);
    assert(info.queue_family_count > 0 && info.memory_heap_count > 0);
    assert((info.strict_profile_supported != 0) == (strcmp(argv[1], "strict") == 0));
    nisyros_plugin_memory_heap_info heap = {0};
    assert(api.memory_heap_info(instance, 0, 0, &heap) == NISYROS_SUCCESS);
    assert(heap.bytes > 0 && heap.reserved == 0);
    uint32_t family = UINT32_MAX;
    for (uint32_t i = 0; i < info.queue_family_count; ++i) {
        nisyros_plugin_queue_family_info queue = {0};
        assert(api.queue_family_info(instance, 0, i, &queue) == NISYROS_SUCCESS);
        if ((queue.flags & NISYROS_QUEUE_COMPUTE) && queue.count) family = i;
    }
    assert(family != UINT32_MAX);
    for (uint32_t i = 0; i < info.cooperative_matrix_count; ++i) {
        nisyros_cooperative_matrix_tuple tuple;
        assert(api.cooperative_matrix(instance, 0, i, &tuple) == NISYROS_SUCCESS);
        assert(tuple.m && tuple.n && tuple.k);
    }
    nisyros_plugin_queue_request request = {family, 1, 1.0f};
    void *device = NULL;
    nisyros_status opened = api.device_open(instance, 0, 1, &request, &device);
    if (info.strict_profile_supported) {
        assert(opened == NISYROS_SUCCESS && device != NULL);
        api.device_close(device);
    } else {
        assert(opened == NISYROS_UNSUPPORTED && device == NULL);
    }
    api.instance_close(instance);
    assert(errors == 0);
    puts("linked provider table negotiated discovery and device opening");
    return 0;
}

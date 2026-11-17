// Copyright 2026, frame-shell contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Vulkan layer measuring the GPU time of an app's frames: every queue submission is bracketed by two GPU
// timestamps, and the OpenXR runtime in the same process ends frames and collects their time through
// vkFrameGpuTimeEnd/vkFrameGpuTimePop (vkGetDeviceProcAddr, which only passes vk names down). Passes everything through until the runtime asks.

#include <vulkan/vk_layer.h>
#include <vulkan/vulkan.h>

#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

#define SLOTS 256 // submissions in flight per queue family
#define FRAMES 16 // frames in flight
#define MAX_FAMILIES 8
#define MAX_QUEUES 16
#define STACK_BATCHES 30

#define KEY(handle) (*(void **)(handle))

enum slot_state
{
	SLOT_FREE,
	SLOT_PENDING,
	SLOT_FAILED,
};

struct family
{
	VkCommandPool pool;
	VkQueryPool queries;
	// [0] resets the slot's queries and stamps the start, [1] stamps the end
	VkCommandBuffer cmds[SLOTS][2];
	enum slot_state state[SLOTS];
	uint64_t frame_seq[SLOTS];
	// The slot's previous end stamp: results still showing it predate the reset
	uint64_t prev_end[SLOTS];
	uint64_t mask;
	uint32_t head, tail;
};

struct frame
{
	uint64_t seq;
	int64_t id;
	uint64_t ticks;
	uint32_t pending;
};

struct instance
{
	struct instance *next;
	VkInstance handle;
	PFN_vkGetInstanceProcAddr gipa;
	PFN_vkDestroyInstance DestroyInstance;
	PFN_vkGetPhysicalDeviceProperties GetPhysicalDeviceProperties;
	PFN_vkGetPhysicalDeviceQueueFamilyProperties GetPhysicalDeviceQueueFamilyProperties;
};

struct device
{
	struct device *next;
	VkDevice handle;
	VkPhysicalDevice physical;
	struct instance *instance;
	PFN_vkSetDeviceLoaderData set_loader_data;

	PFN_vkGetDeviceProcAddr gdpa;
	PFN_vkDestroyDevice DestroyDevice;
	PFN_vkQueueSubmit QueueSubmit;
	PFN_vkQueueSubmit2 QueueSubmit2;
	PFN_vkQueueSubmit2 QueueSubmit2KHR;
	PFN_vkCreateCommandPool CreateCommandPool;
	PFN_vkDestroyCommandPool DestroyCommandPool;
	PFN_vkAllocateCommandBuffers AllocateCommandBuffers;
	PFN_vkBeginCommandBuffer BeginCommandBuffer;
	PFN_vkEndCommandBuffer EndCommandBuffer;
	PFN_vkCreateQueryPool CreateQueryPool;
	PFN_vkDestroyQueryPool DestroyQueryPool;
	PFN_vkCmdResetQueryPool CmdResetQueryPool;
	PFN_vkCmdWriteTimestamp CmdWriteTimestamp;
	PFN_vkGetQueryPoolResults GetQueryPoolResults;

	struct
	{
		VkQueue handle;
		uint32_t family;
	} queues[MAX_QUEUES];
	uint32_t queue_count;

	atomic_bool active;
	pthread_mutex_t mutex; // everything below
	struct family *families[MAX_FAMILIES];
	double ns_per_tick;
	uint64_t busy_until; // end stamp of the latest counted submission
	struct frame frames[FRAMES];
	uint64_t current_seq, pop_seq;
};

static pthread_rwlock_t lock = PTHREAD_RWLOCK_INITIALIZER;
static struct instance *instances;
static struct device *devices;

static struct instance *
find_instance(void *key)
{
	pthread_rwlock_rdlock(&lock);
	struct instance *i = instances;
	while (i != NULL && KEY(i->handle) != key) {
		i = i->next;
	}
	pthread_rwlock_unlock(&lock);
	return i;
}

static struct device *
find_device(void *key)
{
	pthread_rwlock_rdlock(&lock);
	struct device *d = devices;
	while (d != NULL && KEY(d->handle) != key) {
		d = d->next;
	}
	pthread_rwlock_unlock(&lock);
	return d;
}

static struct frame *
get_frame(struct device *d, uint64_t seq)
{
	struct frame *f = &d->frames[seq % FRAMES];
	return f->seq == seq ? f : NULL;
}

// Collects the finished submissions' stamps into their frames
static void
resolve(struct device *d)
{
	for (uint32_t i = 0; i < MAX_FAMILIES; i++) {
		struct family *fam = d->families[i];
		for (; fam != NULL && fam->tail != fam->head; fam->tail = (fam->tail + 1) % SLOTS) {
			uint32_t slot = fam->tail;
			struct frame *f = get_frame(d, fam->frame_seq[slot]);
			if (fam->state[slot] == SLOT_PENDING) {
				// begin, available, end, available
				uint64_t r[4] = {0};
				VkResult res = d->GetQueryPoolResults(
				    d->handle, fam->queries, slot * 2, 2, sizeof(r), r, sizeof(uint64_t) * 2,
				    VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WITH_AVAILABILITY_BIT);
				uint64_t begin = r[0] & fam->mask, end = r[2] & fam->mask;
				if (res < 0) {
					end = begin = 0; // device lost: nothing left to wait for
				} else if (!r[1] || !r[3] || end == fam->prev_end[slot]) {
					break;
				}
				fam->prev_end[slot] = end;
				// Submissions overlap on the GPU's clock when queues run in parallel
				if (begin < d->busy_until) {
					begin = d->busy_until;
				}
				if (end > begin) {
					if (f != NULL) {
						f->ticks += end - begin;
					}
					d->busy_until = end;
				}
			}
			if (f != NULL) {
				f->pending--;
			}
			fam->state[slot] = SLOT_FREE;
		}
	}
}

static struct family *
find_family(struct device *d, VkQueue queue)
{
	for (uint32_t i = 0; i < d->queue_count; i++) {
		if (d->queues[i].handle == queue) {
			return d->families[d->queues[i].family];
		}
	}
	return NULL;
}

// Reserves a slot for one submission to this queue, false when it stays unmeasured
static bool
take_slot(struct device *d, VkQueue queue, struct family **out_fam, uint32_t *out_slot)
{
	if (!atomic_load(&d->active)) {
		return false;
	}
	pthread_mutex_lock(&d->mutex);
	resolve(d);
	struct family *fam = find_family(d, queue);
	bool ok = fam != NULL && (fam->head + 1) % SLOTS != fam->tail;
	if (ok) {
		*out_fam = fam;
		*out_slot = fam->head;
		fam->state[fam->head] = SLOT_PENDING;
		fam->frame_seq[fam->head] = d->current_seq;
		get_frame(d, d->current_seq)->pending++;
		fam->head = (fam->head + 1) % SLOTS;
	}
	pthread_mutex_unlock(&d->mutex);
	return ok;
}

static void
fail_slot(struct device *d, struct family *fam, uint32_t slot)
{
	pthread_mutex_lock(&d->mutex);
	fam->state[slot] = SLOT_FAILED;
	pthread_mutex_unlock(&d->mutex);
}

static VKAPI_CALL VkResult
layer_QueueSubmit(VkQueue queue, uint32_t count, const VkSubmitInfo *submits, VkFence fence)
{
	struct device *d = find_device(KEY(queue));
	struct family *fam;
	uint32_t slot;
	if (count == 0 || !take_slot(d, queue, &fam, &slot)) {
		return d->QueueSubmit(queue, count, submits, fence);
	}

	VkSubmitInfo stack[STACK_BATCHES + 2];
	VkSubmitInfo *all = count <= STACK_BATCHES ? stack : malloc(sizeof(VkSubmitInfo) * (count + 2));
	for (uint32_t i = 0; i < 2; i++) {
		all[i * (count + 1)] = (VkSubmitInfo){
		    .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
		    .commandBufferCount = 1,
		    .pCommandBuffers = &fam->cmds[slot][i],
		};
	}
	memcpy(all + 1, submits, sizeof(VkSubmitInfo) * count);
	VkResult res = d->QueueSubmit(queue, count + 2, all, fence);
	if (all != stack) {
		free(all);
	}
	if (res != VK_SUCCESS) {
		fail_slot(d, fam, slot);
	}
	return res;
}

static VkResult
submit2(PFN_vkQueueSubmit2 next, struct device *d, VkQueue queue, uint32_t count, const VkSubmitInfo2 *submits, VkFence fence)
{
	struct family *fam;
	uint32_t slot;
	if (count == 0 || !take_slot(d, queue, &fam, &slot)) {
		return next(queue, count, submits, fence);
	}

	VkCommandBufferSubmitInfo cmds[2];
	VkSubmitInfo2 stack[STACK_BATCHES + 2];
	VkSubmitInfo2 *all = count <= STACK_BATCHES ? stack : malloc(sizeof(VkSubmitInfo2) * (count + 2));
	for (uint32_t i = 0; i < 2; i++) {
		cmds[i] = (VkCommandBufferSubmitInfo){
		    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
		    .commandBuffer = fam->cmds[slot][i],
		};
		all[i * (count + 1)] = (VkSubmitInfo2){
		    .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
		    .commandBufferInfoCount = 1,
		    .pCommandBufferInfos = &cmds[i],
		};
	}
	memcpy(all + 1, submits, sizeof(VkSubmitInfo2) * count);
	VkResult res = next(queue, count + 2, all, fence);
	if (all != stack) {
		free(all);
	}
	if (res != VK_SUCCESS) {
		fail_slot(d, fam, slot);
	}
	return res;
}

static VKAPI_CALL VkResult
layer_QueueSubmit2(VkQueue queue, uint32_t count, const VkSubmitInfo2 *submits, VkFence fence)
{
	struct device *d = find_device(KEY(queue));
	return submit2(d->QueueSubmit2, d, queue, count, submits, fence);
}

static VKAPI_CALL VkResult
layer_QueueSubmit2KHR(VkQueue queue, uint32_t count, const VkSubmitInfo2 *submits, VkFence fence)
{
	struct device *d = find_device(KEY(queue));
	return submit2(d->QueueSubmit2KHR, d, queue, count, submits, fence);
}

static void
destroy_family(struct device *d, struct family *fam)
{
	if (fam == NULL) {
		return;
	}
	if (fam->pool != VK_NULL_HANDLE) {
		d->DestroyCommandPool(d->handle, fam->pool, NULL);
	}
	if (fam->queries != VK_NULL_HANDLE) {
		d->DestroyQueryPool(d->handle, fam->queries, NULL);
	}
	free(fam);
}

static struct family *
create_family(struct device *d, uint32_t index, uint32_t valid_bits)
{
	struct family *fam = calloc(1, sizeof(*fam));
	fam->mask = valid_bits >= 64 ? UINT64_MAX : (UINT64_C(1) << valid_bits) - 1;

	VkCommandPoolCreateInfo pool_info = {
	    .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
	    .queueFamilyIndex = index,
	};
	VkQueryPoolCreateInfo query_info = {
	    .sType = VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO,
	    .queryType = VK_QUERY_TYPE_TIMESTAMP,
	    .queryCount = SLOTS * 2,
	};
	if (d->CreateCommandPool(d->handle, &pool_info, NULL, &fam->pool) != VK_SUCCESS ||
	    d->CreateQueryPool(d->handle, &query_info, NULL, &fam->queries) != VK_SUCCESS) {
		destroy_family(d, fam);
		return NULL;
	}
	VkCommandBufferAllocateInfo alloc_info = {
	    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
	    .commandPool = fam->pool,
	    .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
	    .commandBufferCount = SLOTS * 2,
	};
	if (d->AllocateCommandBuffers(d->handle, &alloc_info, &fam->cmds[0][0]) != VK_SUCCESS) {
		destroy_family(d, fam);
		return NULL;
	}
	VkCommandBufferBeginInfo begin_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
	for (uint32_t slot = 0; slot < SLOTS; slot++) {
		VkCommandBuffer *cmds = fam->cmds[slot];
		// Command buffers a layer allocates miss the loader's dispatch pointer
		d->set_loader_data(d->handle, cmds[0]);
		d->set_loader_data(d->handle, cmds[1]);

		d->BeginCommandBuffer(cmds[0], &begin_info);
		d->CmdResetQueryPool(cmds[0], fam->queries, slot * 2, 2);
		d->CmdWriteTimestamp(cmds[0], VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, fam->queries, slot * 2);
		d->EndCommandBuffer(cmds[0]);

		d->BeginCommandBuffer(cmds[1], &begin_info);
		d->CmdWriteTimestamp(cmds[1], VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, fam->queries, slot * 2 + 1);
		d->EndCommandBuffer(cmds[1]);
	}
	return fam;
}

// Starts measuring: the pools and pre-recorded stamp commands of every queue family the app uses
static void
activate(struct device *d)
{
	struct instance *inst = d->instance;
	VkPhysicalDeviceProperties props;
	inst->GetPhysicalDeviceProperties(d->physical, &props);
	d->ns_per_tick = props.limits.timestampPeriod;

	VkQueueFamilyProperties families[MAX_FAMILIES];
	uint32_t family_count = MAX_FAMILIES;
	inst->GetPhysicalDeviceQueueFamilyProperties(d->physical, &family_count, families);

	for (uint32_t i = 0; i < d->queue_count; i++) {
		uint32_t index = d->queues[i].family;
		if (index < family_count && d->families[index] == NULL && families[index].timestampValidBits > 0) {
			d->families[index] = create_family(d, index, families[index].timestampValidBits);
		}
	}
	d->frames[0] = (struct frame){0};
	atomic_store(&d->active, true);
}

// Ends the frame whose work the app submitted since the last call; the first call only starts measuring
static VKAPI_CALL void
layer_frameGpuTimeEnd(VkDevice device, int64_t frame_id)
{
	struct device *d = find_device(KEY(device));
	pthread_mutex_lock(&d->mutex);
	if (!atomic_load(&d->active)) {
		activate(d);
		pthread_mutex_unlock(&d->mutex);
		return;
	}
	get_frame(d, d->current_seq)->id = frame_id;
	d->current_seq++;
	d->frames[d->current_seq % FRAMES] = (struct frame){.seq = d->current_seq};
	// The oldest uncollected frame just got overwritten
	if (d->current_seq - d->pop_seq >= FRAMES) {
		d->pop_seq = d->current_seq - FRAMES + 1;
	}
	pthread_mutex_unlock(&d->mutex);
}

// Returns the oldest ended frame once the GPU finished all of its work
static VKAPI_CALL VkBool32
layer_frameGpuTimePop(VkDevice device, int64_t *out_frame_id, uint64_t *out_gpu_time_ns)
{
	struct device *d = find_device(KEY(device));
	VkBool32 ret = VK_FALSE;
	pthread_mutex_lock(&d->mutex);
	if (atomic_load(&d->active)) {
		resolve(d);
		struct frame *f = get_frame(d, d->pop_seq);
		if (d->pop_seq < d->current_seq && f->pending == 0) {
			*out_frame_id = f->id;
			*out_gpu_time_ns = (uint64_t)((double)f->ticks * d->ns_per_tick);
			d->pop_seq++;
			ret = VK_TRUE;
		}
	}
	pthread_mutex_unlock(&d->mutex);
	return ret;
}

static VKAPI_CALL void
layer_DestroyDevice(VkDevice device, const VkAllocationCallbacks *alloc)
{
	if (device == VK_NULL_HANDLE) {
		return;
	}
	pthread_rwlock_wrlock(&lock);
	struct device **p = &devices;
	while (KEY((*p)->handle) != KEY(device)) {
		p = &(*p)->next;
	}
	struct device *d = *p;
	*p = d->next;
	pthread_rwlock_unlock(&lock);

	for (uint32_t i = 0; i < MAX_FAMILIES; i++) {
		destroy_family(d, d->families[i]);
	}
	d->DestroyDevice(device, alloc);
	pthread_mutex_destroy(&d->mutex);
	free(d);
}

static VKAPI_CALL VkResult
layer_CreateDevice(VkPhysicalDevice physical,
                   const VkDeviceCreateInfo *info,
                   const VkAllocationCallbacks *alloc,
                   VkDevice *out_device)
{
	VkLayerDeviceCreateInfo *link = NULL;
	PFN_vkSetDeviceLoaderData set_loader_data = NULL;
	for (VkBaseOutStructure *s = (VkBaseOutStructure *)info->pNext; s != NULL; s = s->pNext) {
		VkLayerDeviceCreateInfo *l = (VkLayerDeviceCreateInfo *)s;
		if (s->sType != VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO) {
			continue;
		}
		if (l->function == VK_LAYER_LINK_INFO) {
			link = l;
		} else if (l->function == VK_LOADER_DATA_CALLBACK) {
			set_loader_data = l->u.pfnSetDeviceLoaderData;
		}
	}
	struct instance *inst = find_instance(KEY(physical));
	if (link == NULL || inst == NULL) {
		return VK_ERROR_INITIALIZATION_FAILED;
	}
	PFN_vkGetInstanceProcAddr gipa = link->u.pLayerInfo->pfnNextGetInstanceProcAddr;
	PFN_vkGetDeviceProcAddr gdpa = link->u.pLayerInfo->pfnNextGetDeviceProcAddr;
	link->u.pLayerInfo = link->u.pLayerInfo->pNext;

	PFN_vkCreateDevice create = (PFN_vkCreateDevice)gipa(inst->handle, "vkCreateDevice");
	VkResult res = create(physical, info, alloc, out_device);
	if (res != VK_SUCCESS) {
		return res;
	}

	struct device *d = calloc(1, sizeof(*d));
	d->handle = *out_device;
	d->physical = physical;
	d->instance = inst;
	d->set_loader_data = set_loader_data;
	d->gdpa = gdpa;
	pthread_mutex_init(&d->mutex, NULL);
#define GET(name) d->name = (PFN_vk##name)gdpa(d->handle, "vk" #name)
	GET(DestroyDevice);
	GET(QueueSubmit);
	GET(QueueSubmit2);
	d->QueueSubmit2KHR = (PFN_vkQueueSubmit2)gdpa(d->handle, "vkQueueSubmit2KHR");
	GET(CreateCommandPool);
	GET(DestroyCommandPool);
	GET(AllocateCommandBuffers);
	GET(BeginCommandBuffer);
	GET(EndCommandBuffer);
	GET(CreateQueryPool);
	GET(DestroyQueryPool);
	GET(CmdResetQueryPool);
	GET(CmdWriteTimestamp);
	GET(GetQueryPoolResults);
#undef GET

	// Protected queues (flags set) stay unmeasured
	PFN_vkGetDeviceQueue get_queue = (PFN_vkGetDeviceQueue)gdpa(d->handle, "vkGetDeviceQueue");
	for (uint32_t i = 0; i < info->queueCreateInfoCount; i++) {
		const VkDeviceQueueCreateInfo *q = &info->pQueueCreateInfos[i];
		for (uint32_t j = 0; j < q->queueCount && q->flags == 0 && d->queue_count < MAX_QUEUES; j++) {
			get_queue(d->handle, q->queueFamilyIndex, j, &d->queues[d->queue_count].handle);
			d->queues[d->queue_count++].family = q->queueFamilyIndex;
		}
	}

	pthread_rwlock_wrlock(&lock);
	d->next = devices;
	devices = d;
	pthread_rwlock_unlock(&lock);
	return VK_SUCCESS;
}

static VKAPI_CALL void
layer_DestroyInstance(VkInstance instance, const VkAllocationCallbacks *alloc)
{
	if (instance == VK_NULL_HANDLE) {
		return;
	}
	pthread_rwlock_wrlock(&lock);
	struct instance **p = &instances;
	while (KEY((*p)->handle) != KEY(instance)) {
		p = &(*p)->next;
	}
	struct instance *inst = *p;
	*p = inst->next;
	pthread_rwlock_unlock(&lock);

	inst->DestroyInstance(instance, alloc);
	free(inst);
}

static VKAPI_CALL VkResult
layer_CreateInstance(const VkInstanceCreateInfo *info, const VkAllocationCallbacks *alloc, VkInstance *out_instance)
{
	VkLayerInstanceCreateInfo *link = (VkLayerInstanceCreateInfo *)info->pNext;
	while (link != NULL &&
	       (link->sType != VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO || link->function != VK_LAYER_LINK_INFO)) {
		link = (VkLayerInstanceCreateInfo *)link->pNext;
	}
	if (link == NULL) {
		return VK_ERROR_INITIALIZATION_FAILED;
	}
	PFN_vkGetInstanceProcAddr gipa = link->u.pLayerInfo->pfnNextGetInstanceProcAddr;
	link->u.pLayerInfo = link->u.pLayerInfo->pNext;

	PFN_vkCreateInstance create = (PFN_vkCreateInstance)gipa(VK_NULL_HANDLE, "vkCreateInstance");
	VkResult res = create(info, alloc, out_instance);
	if (res != VK_SUCCESS) {
		return res;
	}

	struct instance *inst = calloc(1, sizeof(*inst));
	inst->handle = *out_instance;
	inst->gipa = gipa;
#define GET(name) inst->name = (PFN_vk##name)gipa(inst->handle, "vk" #name)
	GET(DestroyInstance);
	GET(GetPhysicalDeviceProperties);
	GET(GetPhysicalDeviceQueueFamilyProperties);
#undef GET

	pthread_rwlock_wrlock(&lock);
	inst->next = instances;
	instances = inst;
	pthread_rwlock_unlock(&lock);
	return VK_SUCCESS;
}

static VKAPI_CALL PFN_vkVoidFunction
layer_GetDeviceProcAddr(VkDevice device, const char *name);

static PFN_vkVoidFunction
device_function(struct device *d, const char *name)
{
#define HOOK(fn)                                                                                                       \
	if (strcmp(name, "vk" #fn) == 0) {                                                                             \
		return (PFN_vkVoidFunction)layer_##fn;                                                                 \
	}
	HOOK(GetDeviceProcAddr);
	HOOK(DestroyDevice);
	HOOK(QueueSubmit);
	// Only where the device has them
	if (d == NULL || d->QueueSubmit2 != NULL) {
		HOOK(QueueSubmit2);
	}
	if (d == NULL || d->QueueSubmit2KHR != NULL) {
		HOOK(QueueSubmit2KHR);
	}
#undef HOOK
	if (strcmp(name, "vkFrameGpuTimeEnd") == 0) {
		return (PFN_vkVoidFunction)layer_frameGpuTimeEnd;
	}
	if (strcmp(name, "vkFrameGpuTimePop") == 0) {
		return (PFN_vkVoidFunction)layer_frameGpuTimePop;
	}
	return NULL;
}

static VKAPI_CALL PFN_vkVoidFunction
layer_GetDeviceProcAddr(VkDevice device, const char *name)
{
	struct device *d = find_device(KEY(device));
	PFN_vkVoidFunction fn = device_function(d, name);
	return fn != NULL ? fn : d->gdpa(device, name);
}

static VKAPI_CALL PFN_vkVoidFunction
layer_GetInstanceProcAddr(VkInstance instance, const char *name)
{
#define HOOK(fn)                                                                                                       \
	if (strcmp(name, "vk" #fn) == 0) {                                                                             \
		return (PFN_vkVoidFunction)layer_##fn;                                                                 \
	}
	HOOK(GetInstanceProcAddr);
	HOOK(CreateInstance);
	HOOK(DestroyInstance);
	HOOK(CreateDevice);
#undef HOOK
	if (instance == VK_NULL_HANDLE) {
		return NULL;
	}
	struct instance *inst = find_instance(KEY(instance));
	PFN_vkVoidFunction next = inst->gipa(instance, name);
	// Device functions asked through the instance: ours only where the driver has them too
	PFN_vkVoidFunction fn = next != NULL && strncmp(name, "vk", 2) == 0 ? device_function(NULL, name) : NULL;
	return fn != NULL ? fn : next;
}

__attribute__((visibility("default"))) VKAPI_CALL VkResult
frame_gpu_time_negotiate(VkNegotiateLayerInterface *interface)
{
	if (interface->loaderLayerInterfaceVersion < 2) {
		return VK_ERROR_INITIALIZATION_FAILED;
	}
	interface->loaderLayerInterfaceVersion = 2;
	interface->pfnGetInstanceProcAddr = layer_GetInstanceProcAddr;
	interface->pfnGetDeviceProcAddr = layer_GetDeviceProcAddr;
	interface->pfnGetPhysicalDeviceProcAddr = NULL;
	return VK_SUCCESS;
}

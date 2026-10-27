// SPDX-License-Identifier: MIT
// The frame shell's C ABI, the one definition for both languages: WiVRn's client and WayVR (libwayvr.so) on the
// headset, WiVRn's server and the PC host services (libframe_host.a). Rust gets its bindings from this header
// through bindgen (the frame-abi crate). Messages passed as bytes are wayvr_ipc::frame's, serialized by Rust.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FRAME_SHELL_ABI_VERSION 6

// ---- Headset: WiVRn client <-> WayVR ----

// PC input from the shell; pointer positions (kind 2) are normalized on (source, surface)
typedef struct frame_shell_input
{
	uint32_t kind, code;
	float x, y;
	uint32_t source, surface;
} frame_shell_input;

// A PC monitor (surface) of a PC (source) appearing, changing size or going away
typedef struct frame_shell_desktop_surface
{
	uint32_t source, surface, width, height;
	bool removed;
	char name[64], source_name[64];
} frame_shell_desktop_surface;

// A decoded desktop frame: NV12 in one DMA-buf, chroma after the luma rows. The decoder doesn't reuse buffer `id`
// until the shell gives it back with frame_shell_host::release_desktop_frame; the fd is borrowed for the call.
typedef struct frame_shell_desktop_frame
{
	uint32_t source, surface;
	int32_t fd;
	uint32_t width, height, stride;
	uint64_t chroma_offset, id;
} frame_shell_desktop_frame;

// Connection stage is independent of discovery, build and XR presentation.
typedef enum frame_pc_connection {
    FRAME_PC_DISCONNECTED, FRAME_PC_CONNECTING, FRAME_PC_PAIRING, FRAME_PC_CONNECTED, FRAME_PC_ERROR
} frame_pc_connection;

typedef struct frame_shell_pc {
    const char * id, * name, * hostname, * build, * version, * detail;
    // Its stream settings, JSON
    const char * settings;
    uint32_t source;
    frame_pc_connection connection;
    bool available, compatible, autoconnect, manual, background_pinned;
} frame_shell_pc;

typedef struct frame_runtime_client {
    uint32_t id;
    bool overlay, visible, primary;
} frame_runtime_client;

// What the client offers the shell, callable from any thread while run() hasn't returned
typedef struct frame_shell_host
{
	void * user;
	// The shell no longer reads a desktop frame
	void (*release_desktop_frame)(void * user, uint64_t id);
	// A wayvr_ipc::frame::ShellMessage for a connected PC (source)
	void (*send_host_message)(void * user, uint32_t source, const uint8_t * data, size_t size);
	// Resolved OpenXR input paths captured by the shell; strings are borrowed for this call
	void (*set_input_capture)(void * user, const char * const * paths, size_t count, uint32_t pose_mask);
    // Effective runtime clients; unknown state must not be interpreted as all applications exiting
    void (*runtime_clients)(void * user, uint32_t source, const frame_runtime_client * clients, size_t count, bool known, bool composition_supported);
} frame_shell_host;

// The shell's entry points, from frame_shell_api() in libwayvr.so
typedef struct frame_shell_api
{
	uint32_t version; // FRAME_SHELL_ABI_VERSION
	// Runs the shell until stop(); non-zero on failure
	int32_t (*run)(const frame_shell_host * host);
	void (*stop)(void);
	bool (*input_poll)(frame_shell_input * event);
	void (*desktop_surface)(const frame_shell_desktop_surface * info);
	void (*desktop_frame)(const frame_shell_desktop_frame * frame);
	// A PC (source) connected or went away, or changed role; vr: it presents VR, else it is on standby
	void (*connected)(uint32_t source, bool connected, bool vr);
	// Lobby (servers, connection, pairing) and stream (connected server, PC apps) state for the UI, as JSON
	void (*lobby_state)(const char * json);
	void (*stream_state)(uint32_t source, const char * json);
	// A wayvr_ipc::frame::HostMessage from a connected PC (source)
	void (*host_message)(uint32_t source, const uint8_t * data, size_t size);
    // Complete discovery/connection snapshot; selected_source chooses the remote background, not overlay ownership
    void (*hosts_state)(const frame_shell_pc * pcs, size_t count, uint32_t selected_source);
	// Next command line for the client ("connect <cookie>", "setting <key> <value>", ...); false when there is none
	bool (*command_poll)(char * buffer, size_t size);
	// The shell's temporary space drag as floor-from-stage: qx, qy, qz, qw, x, y, z
	void (*drag_offset)(float * out);
} frame_shell_api;

typedef const frame_shell_api * (*frame_shell_api_fn)(void);

// ---- PC: WiVRn server <-> host services ----

// A streamed monitor: encoded size, where it lies in the whole desktop (x, y, w, h as fractions, for pointer
// input), output name and codec (0 H.264, 1 HEVC: WiVRn's video_codec)
typedef struct frame_host_surface
{
	uint32_t id, width, height;
	float region[4];
	const char * name;
	uint32_t codec;
} frame_host_surface;

// Called from the host's threads; data is borrowed for the call
typedef struct frame_host_callbacks
{
	void * user;
	// Announced before a monitor's first frame and again when its size or codec changes
	void (*surface)(void * user, const frame_host_surface * surface);
	void (*surface_removed)(void * user, uint32_t id);
	// One encoded access unit
	void (*video)(void * user, uint32_t surface, const uint8_t * data, size_t size);
	// A wayvr_ipc::frame::HostMessage for the shell
	void (*message)(void * user, const uint8_t * data, size_t size);
} frame_host_callbacks;

typedef struct frame_host_identity {
    const char * id, * name, * build, * version;
} frame_host_identity;

typedef struct frame_host frame_host;
// Starts the host services for one headset connection; null on failure
frame_host * frame_host_start(const frame_host_callbacks * callbacks, const frame_host_identity * identity);
// A wayvr_ipc::frame::ShellMessage from the shell
void frame_host_receive(frame_host * host, const uint8_t * data, size_t size);
// The headset lost part of a monitor's video and waits for its next keyframe
void frame_host_request_keyframe(frame_host * host, uint32_t surface);
// Stops the services and frees the host; no callback runs after it returns
void frame_host_stop(frame_host * host);

#ifdef __cplusplus
}
#endif

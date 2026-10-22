# How the pieces communicate

## Processes

- **PC:** `wivrn-server` links the host services (`desktop/`, a Rust static library). WiVRn owns the PC's OpenXR
  runtime, composition, encoding, tracking and input; the host services capture monitors through the
  screen-sharing portal, report metrics, games, runtime clients and notifications, and tunnel WayVR IPC clients.
  Each session runs in a process `posix_spawn`ed by the listener (no forked GLib/GStreamer state).
- **Headset:** Monado runs separately. `frame-shell-client` is the WiVRn client and loads `libwayvr.so` on its own
  thread. It keeps one `stream` per connected PC (the shell's source number); the PC whose VR app started last
  presents, the others stand by (`video_active` off). WiVRn decodes the VR and desktop streams; WayVR lays out
  desktops, dashboard, keyboard and watch. Each has its own OpenXR session and Vulkan device; Monado composes
  their layers with passthrough.

## C ABI

`wayvr/frame-abi/include/frame_abi.h` defines both ABIs; the `frame-abi` crate generates the Rust side.

- Headset: `libwayvr.so` exports `frame_shell_api()`. The client passes `frame_shell_host` callbacks: releasing
  desktop frames, sending messages to a PC. Lobby and stream state go to WayVR as JSON; WayVR's commands for
  the client are text lines from `command_poll`.
- PC: `frame_host_start`/`frame_host_stop` run the host services for one connection (callbacks for surfaces,
  video and messages); stop joins every thread.

## Shell ↔ host

`wayvr_ipc::frame::{HostMessage, ShellMessage}`, carried by WiVRn as opaque `shell_message` bytes on the control
connection: stats, games, runtime clients, frame timings and notifications one way; desktop settings,
visibility, focus and game stop the other. Every shell event names its PC.

The host serves WayVR's IPC socket on the PC and tunnels each client to the headset (`Ipc*` messages); starting
or stopping headset processes is refused. Watch controls acting on the PC send `CustomEvent`s to subscribed
clients (`::IpcSend`), which answer with `wayvrctl panel-modify`.

## Desktop video

One portal session, restore token and hardware H.264/HEVC encoder per monitor. `desktop_surface` announces a
monitor (codec, size, region in the desktop for pointer input) before its first frame. Access units travel as
`desktop_video` shards on the stream socket, with FEC parity and resends on request (`desktop_nack`); a frame
that can't be completed drops frames until the next keyframe (`desktop_keyframe`). Hidden monitors aren't encoded.
Decoded frames are NV12 DMA-bufs that stay out of the decoder until WayVR releases them.

VR frames carry a 32-byte foveation description; both ends build the mapping tables with
`wivrn/common/wivrn_foveation.cpp`, and the client passes the foveated image to Monado
(`XR_FRAME_foveated_projection`).

## External PC interfaces

- WayVR IPC: abstract socket `/tmp/wayvr_ipc.sock`, length-prefixed JSON (u32 big-endian), protocol 6.
- Notifications: UDP `127.0.0.1:42069` (XSOverlay format) and `org.freedesktop.Notifications` (D-Bus monitor).
- libmonado: metrics fd (length-delimited protobuf), client list with process IDs, `set_client_primary`.
- Host load, RAM, temperature and GPU power: sysinfo and AMD sysfs. Steam games: launch metadata in `/proc`.

Host workers poll on their own threads; nothing on the headset's render thread waits for the PC.

## State

Stream settings are the headset's WiVRn configuration and apply to every PC (bitrate in `settings_changed`;
sizes and codecs in `stream_settings`, which rebuild the server's images, foveation and encoders). Pairing and
auto-connect are per PC; window placement and hidden states are WayVR's, keyed by PC/monitor names. The client
locates poses in the calibrated space (floor and recenter) and sends the shell's temporary space drag as the
tracking origin offset, so PC games and trackers move together.

Stream replacement waits for submitted frames before freeing images; invalid stream setup is fatal, no rollback.

# How the pieces communicate

## Processes

- **PC:** `wivrn-server` links the host services (`desktop/`, a Rust static library). WiVRn owns the PC's OpenXR
  runtime, composition, encoding, tracking and input; the host services capture monitors through the
  screen-sharing portal, report metrics, games, runtime clients and notifications, and tunnel WayVR IPC clients.
  Each session runs in a process `posix_spawn`ed by the listener (no forked GLib/GStreamer state).
- **Headset:** Monado runs separately. `frame-shell-client` is the WiVRn client and loads `libwayvr.so` on its own
  thread. It keeps one `stream` per connected PC (a transient source number) and composes their VR streams into
  one OpenXR frame: the background PC first, then other PCs' overlays (their runtime hides their background app).
  WiVRn decodes the VR and desktop streams; WayVR lays out
  desktops, dashboard, keyboard and watch. Each has its own OpenXR session and Vulkan device; Monado composes
  their layers with passthrough. Opaque apps select the default background; apps that submit alpha-blended
  frames become independently toggleable overlays for the session. Both runtimes expose the same presentation
  controls through libmonado: automatic, default app, or overlay, plus visibility. The keyboard top row
  uses left click for selection/toggling and right click for presentation, stop and unpin actions.

## C ABI

`wayvr/frame-abi/include/frame_abi.h` defines both ABIs; the `frame-abi` crate generates the Rust side.

- Headset: `libwayvr.so` exports `frame_shell_api()`. The client passes `frame_shell_host` callbacks: releasing
  desktop frames, sending messages to a PC, resolved input capture and runtime activity. Connection/discovery snapshots use `frame_shell_pc`, generated for Rust from this header. Settings
  and each connection's installed-app/debug state remain JSON; control commands use `command_poll`.
- PC: `frame_host_start`/`frame_host_stop` run the host services for one connection (callbacks for surfaces,
  video and messages); stop joins every thread.

## Shell ↔ host

`wayvr_ipc::frame::{HostMessage, ShellMessage}`, carried by WiVRn as opaque `shell_message` bytes on the control
connection: stats, games, runtime clients, frame timings and notifications one way; desktop settings,
visibility, focus and game stop the other. Every shell event names its PC.

The host serves WayVR's IPC socket on the PC and tunnels each client to the headset (`Ipc*` messages); starting
or stopping headset processes is refused. Watch controls acting on the PC send `CustomEvent`s to subscribed
clients (`::IpcSend`), which answer with panel-modify. A custom watch (`gui/watch.xml` in WayVR's theme folder)
is driven this way; WayVR itself only fills in its frame timings.

## Input capture

WayVR derives per-hand capture from the active profile and its binding evaluator: primary clicks for the watch,
configured click/grab/scroll inputs for other panels, and global gestures only when consumed. Activation aliases
(click/value/force) are grouped; unconfigured passive sensing and watch-reveal bindings are left alone.

The same path set goes through `frame_shell_host::set_input_capture` to WiVRn before PC input transmission, and
through libmonado's connection-owned `mnd_root_set_input_capture` to native clients. Monado generates physical
input mappings from its OpenXR bindings/rebindings. It filters each client's copy and exempts the owner's process
(WayVR and WiVRn share a PID). Both consumers retain neutral values until a captured held input is released; activation aliases and stick axes release together.
Optional keyboard pose blocking freezes controller poses in both paths.
Closing the control connection clears native capture; stopping the shell clears the in-process policy. There is
no injected API layer or shared focus file.

## Desktop video

One portal session, restore token and hardware H.264/HEVC encoder per monitor. Restore tokens are per app (the
portal derives it from the systemd scope), so the development server runs in its own scope like the installed
service. The cast hands DMA-bufs to VA-API, and the encoder is always told its source is live so it never holds
frames back. `desktop_surface` announces a
monitor (codec, size, region in the desktop for pointer input) before its first frame. Access units travel as
`video_shard` packets on the stream socket, using the same per-stream sharding, FEC and retransmission
implementation as VR video (`wivrn/common/shard_stream.*`). Every shard carries its frame size, shard counts and the
stream's previous frame, so a frame lost whole is told from a frame number the PC skipped; `video_nack` requests
missing shards. VR metadata is serialized before the encoded bytes. Each VR encoder and
monitor owns separate recovery state. VR decodes complete frames immediately; desktops decode in order, waiting
up to 40 ms for recovery. A desktop frame
that can't be completed drops frames until the next keyframe (`desktop_keyframe`). Hidden monitors aren't encoded.
Decoded frames are NV12 DMA-bufs that stay out of the decoder until WayVR releases them.

VR frames carry a 32-byte foveation description; both ends build the mapping tables with
`wivrn/common/wivrn_foveation.cpp`, and the client passes the foveated image to Monado
(`XR_FRAME_foveated_projection`).

## External PC interfaces

- WayVR IPC: abstract socket `/tmp/wayvr_ipc.sock`, length-prefixed JSON (u32 big-endian), protocol 6.
- Notifications: UDP `127.0.0.1:42069` (XSOverlay format) and `org.freedesktop.Notifications` (D-Bus monitor).
- libmonado: metrics fd (length-delimited protobuf), effective client state, presentation and input capture;
  the PC runtime adds background suppression without changing the selected client (API 1.12).
- Steam games: launch metadata in `/proc`.

Host workers poll on their own threads; nothing on the headset's render thread waits for the PC.

## State

Stream settings (render size, foveation, bitrate, codecs, FEC, frame margin, desktop encoding) are per PC in the
headset's WiVRn configuration; a newly found PC starts with the presenting PC's. Bitrate goes in `settings_changed`;
sizes and codecs in `stream_settings`, which rebuild the server's images, foveation and encoders. Pairing and
auto-connect are per PC, identified by its server cookie (shared by the packaged and dev servers through
`FRAME_SHELL_IDENTITY_DIR`); pins and screen windows are keyed by it, source numbers and runtime client IDs only
live as long as a connection. The client locates poses in the calibrated space (floor and recenter) and sends the shell's temporary space drag as the
tracking origin offset, so PC games and trackers move together.

Stream replacement waits for submitted frames before freeing images; invalid stream setup is fatal, no rollback.

`BackgroundVisible` tells a PC's runtime whether to show its selected background app, without changing the app's
presentation mode; the host answers `CompositionSupported` once it applied it. Only such hosts stream overlays
while not the background PC. Tracking goes to every streaming PC, the microphone only to the background PC. The
lobby owns the combined frame: each stream adds its projection layer and sends its frame feedback after submission.

## Debug metrics

`$XDG_RUNTIME_DIR/frame-metrics.sock` on the headset and PC streams records as JSON lines while something reads it
(`wayvr_ipc::metrics`); without a reader they are dropped. Monitors report their stages on each device: `captured`,
`to_encoder`, `keyframe`, `encoded`, `sent` on the PC, `received`, `decoded`, `shown` and losses on the headset. A
repeated `seq` at `captured` is a keepalive, not a new frame from the compositor.

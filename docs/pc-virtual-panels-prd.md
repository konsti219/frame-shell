# PRD & Implementation Guide: Interactive Virtual PC Panels with 3D-to-2D Layout Sync

**Status:** Draft / Proposed  
**Target Branch:** `feat/pc-share-panels`  
**Primary Platform:** Steam Frame (`frame-shell` headset shell) + Linux PC host (`wivrn-server` + `desktop/`), targeting **Hyprland** first with compositor-agnostic protocol seams for Sway, niri, and KWin.  
**Last Updated:** 2026-11-27

---

## 1. Executive Summary

Today Frame Shell only streams physical PC monitors via the XDG `ScreenCast` portal (`capture::run` in [`desktop/src/capture.rs`](../desktop/src/capture.rs)). Capturing individual windows or inactive workspaces through XDG `ScreenCast` runs into three structural Wayland/portal limitations:

1. **All streams freeze during selection:** When `xdg-desktop-portal-hyprland` (XDPH) needs user selection, it launches `hyprland-share-picker` synchronously on its main loop (`execAndGet` / `runSync`). Because the same daemon pumps screencopy frames for every active physical monitor session, opening the picker freezes all streams in VR, forcing the user to remove the headset.
2. **Background framerate throttling:** Windows and workspaces not visible on a physical output stop receiving normal Wayland frame callbacks. Even with Hyprland's `render_unfocused` window rule, unfocused rendering is capped at `misc:render_unfocused_fps` (15 FPS by default), and many applications throttle internally when occluded or unfocused.
3. **No direct mouse/keyboard interaction:** Unmapped windows and inactive workspaces have no coordinates in the active desktop bounding box, so `/dev/uinput` `EV_ABS` events in [`wivrn/server/driver/wivrn_uinput.cpp`](../wivrn/server/driver/wivrn_uinput.cpp) and physical mouse edge-crossing cannot target off-screen surfaces without switching the physical monitor's foreground workspace.

### Proposed Solution

Instead of asking the XDG `ScreenCast` portal to capture hidden windows or inactive workspaces, Frame Shell will create and manage **dynamic headless (virtual) outputs** on the host compositor, move chosen workspaces or windows onto them, capture them directly over Wayland (**`ext-image-copy-capture-v1`**) from an **in-headset picker**, and **automatically sync the VR 3D panel layout back to the PC's 2D monitor geometry**.

When a user grabs a virtual panel in VR and places it **above** their primary monitor, Frame Shell automatically tells Hyprland to position that headless output directly above the primary monitor (for example, `0x-1080`). Moving the physical mouse up glides directly into that window without touching the physical monitor's foreground workspace, at a full 60 Hz, with zero freezing pickers.

---

## 2. Goals, Non-Goals & Success Criteria

### Goals & Measurable Criteria

| Goal | Target / Acceptance Criterion |
| --- | --- |
| **Multiple independent panels** | Support multiple concurrent virtual panels per PC up to a configurable limit (default **3** virtual panels, tunable based on hardware encode/decode limits). |
| **Zero picker freezes** | Adding, configuring, or removing virtual panels happens directly inside Frame Shell (keyboard bar and dashboard); existing monitor streams continue at full frame rate without opening `hyprland-share-picker`. |
| **Full 60 Hz off-monitor rendering** | Workspaces and windows on virtual outputs run as the foreground of an active output, sustaining 60 Hz pacing regardless of what workspace is shown on physical monitors. |
| **Seamless physical mouse, keyboard & VR laser control** | Physical mouse crosses shared edges cleanly into/out of virtual panels; physical keyboard follows the compositor's focus policy into virtual windows; VR laser pointer & virtual keyboard interact via Frame Shell's normal `DesktopOverlay` / `PcInput` path. |
| **Automatic 3D-to-2D layout sync** | Spawning, moving (grab-release), or closing virtual panels updates the host's 2D virtual monitor positions after a short debounce (~300–500 ms) so physical mouse direction matches 3D placement. |
| **Clean teardown / zero desktop residue** | Closing a panel or disconnecting the headset restores windows/workspaces to a physical monitor and removes all headless outputs without altering physical monitor coordinates. |

### Non-Goals (v1)

- **Eye-gaze cursor teleportation ("MAGIC pointing"):** Deferred to Phase 5 as an additive enhancement on top of the 2D layout sync.
- **Replacing PipeWire portal capture for physical monitors:** Existing physical monitor capture (`capture::run` in [`desktop/src/capture.rs`](../desktop/src/capture.rs)) remains PipeWire portal-based for monitors present at session start (cross-compositor for KWin, GNOME, Hyprland, Sway); dynamic tracking updates desktop `regions` without introducing mid-session portal prompts.
- **Depth-stacked or wrap-behind 2D topologies:** Physical 2D monitor canvases have no Z axis; panels stacked behind each other in 3D fall back to nearest non-overlapping edge placement.
- **Cross-PC cursor bridging:** Mouse edge-crossing operates within each PC's own 2D desktop canvas (consistent with `PcInput` in [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs)).

---

## 3. User Flows & UX Requirements

### Flow A: Add a Virtual Screen or Pop Out a Window/Workspace (Directly from Frame Shell)
1. **Primary quick-access entry points on the keyboard bar** ([`wayvr/wayvr/src/assets/gui/keyboard.xml`](../wayvr/wayvr/src/assets/gui/keyboard.xml), [`wayvr/wayvr/src/gui/panel/overlay_list.rs`](../wayvr/wayvr/src/gui/panel/overlay_list.rs)):
   - **Keyboard hamburger menu (`menu_burger`):** Alongside `BAR.ADD_MIRROR` and `BAR.ADD_PASSTHRU`, add an **Add virtual panel** item (hidden when no connected PC supports virtual outputs) that opens a lightweight picker showing the target PC (auto-selected when only one PC supports it) and its available blank screen presets (e.g., `1920×1080`, `2560×1440`, `1280×1440` portrait), open windows, or workspaces.
   - **PC group menu (`menu_host` in `HostGroup`):** Right-clicking a connected PC button in the keyboard top row also exposes **Add virtual panel…** scoped directly to that PC.
   - **Secondary full view — Dashboard → PC (WiVRn) tab** ([`wayvr/dash-frontend/src/tab/wivrn.rs`](../wayvr/dash-frontend/src/tab/wivrn.rs), [`wayvr/dash-frontend/assets/gui/tab/wivrn.xml`](../wayvr/dash-frontend/assets/gui/tab/wivrn.xml)): Houses the full Virtual panels management list, window/workspace picker, and the **Auto-sync VR layout to desktop** setting.
2. User chooses a blank virtual monitor, a PC window, or a workspace.
3. No dialog opens on the PC desktop. Within ~0.5 s:
   - The PC host creates a headless output (`FRAME-V1`), configures its mode/position, and silently migrates the selected window or workspace onto it.
   - The headset spawns a movable, interactive panel (`DesktopOverlay`) beside/above the user's current monitors, and adds its button to that PC's `HostGroup` in the keyboard top row.
   - Existing monitor streams never drop or stall.

### Flow B: Arrange in VR & Control Directly with Physical Mouse/Keyboard
1. User grabs a virtual panel in VR and places it above the main physical monitor overlay, then releases the grab.
2. WayVR triggers a debounced layout sync hook (~300–500 ms after grab release).
3. Frame Shell's 3D-to-2D layout solver computes `FRAME-V1`'s edge-snapped logical pixel position (e.g. `0x-1080` directly above a `1920×1080` screen at `0x0`, keeping the physical monitor anchored).
4. The PC host repositions `FRAME-V1` live and updates pointer `regions` in [`wivrn/server/driver/desktop_link.h`](../wivrn/server/driver/desktop_link.h).
5. User pushes their physical mouse upward off the top of their physical screen: the cursor enters `FRAME-V1`, and physical keyboard focus follows Hyprland's normal focus policy (focusing immediately under the cursor with default `follow_mouse = 1`, or on click under click-to-focus). Moving the mouse back down returns seamlessly to the main monitor.

### Flow C: VR Laser & Virtual Keyboard Interaction
1. Without touching a physical mouse, the user points a VR controller laser at any virtual panel and clicks/scrolls/types.
2. Because the panel has a valid normalized `region` in the PC's global desktop bounding box, Frame Shell's normal interactive `DesktopOverlay` / `PcInput` path injects `ShellInput` events mapped onto that virtual output's coordinates just like physical monitors: hover moves the pointer (`EV_ABS`), clicking focuses the surface under the cursor, and keyboard events from WayVR's virtual keyboard go to the compositor's active focus.

### Flow D: Hide, Close Panel, App Exit, or Disconnect Headset
1. **Hiding a panel (toggle / overlay set switch):** Hiding a panel in VR only pauses its capture and video encoding on the host (`DesktopVisibility { visible: false }`). The headless output stays alive and in place on Hyprland so its windows are not ejected and the 2D layout does not thrash when switching VR sets. (**Keep visible when hidden** keeps the panel visible and streaming as usual.)
2. **Closing a panel / disconnecting:** User right-clicks the virtual panel button in the keyboard top row (`menu_virtual_screen` → **Close virtual panel**) or clicks **Close** in the dashboard's PC tab (or disconnects / shuts down Frame Shell).
   - **`Blank` / `Window` panels:** Return any windows on the panel's dedicated workspace back to a physical monitor (without stealing focus unnecessarily), then remove `FRAME-V*`.
   - **`Workspace` panels:** Move the workspace back to its original physical monitor (fallback: primary physical monitor), then remove `FRAME-V*`.
3. **App exit on PC:** If the user quits the popped-out application directly on the PC, a `Window`-backed panel automatically closes (evacuating any other windows on it and removing `FRAME-V*`); `Workspace` and `Blank` panels remain open until explicitly closed.
4. The headset drops the transient `DesktopOverlay` without persisting it to `frame-shell-screens`. Physical monitors remain in their original positions.

### Flow E (Phase 5 Stretch): Gaze-Guided Cursor Warp
1. Using Frame Shell's calibrated `app.input_state.eye_gaze` ([`wayvr/wayvr/src/backend/openxr/input.rs`](../wayvr/wayvr/src/backend/openxr/input.rs), [`wayvr/wayvr/src/eye_calibration.rs`](../wayvr/wayvr/src/eye_calibration.rs)), the user looks at a far virtual panel and taps a dedicated key/chord.
2. Frame Shell sends the intersected surface UV to the PC host, which warps the cursor to that panel and focuses it — avoiding long mouse drags across multiple virtual screens while preventing accidental focus theft during casual glances.

---

## 4. Technical Architecture

```
Headset (WayVR / frame-shell-client)                     PC Host (wivrn-server + desktop/)
────────────────────────────────────                     ──────────────────────────────────
1. Keyboard Bar & Dashboard UI                           6. CompositorBackend (Hyprland IPC)
   (keyboard.xml, overlay_list.rs, tab/wivrn.rs)            - create/remove headless outputs
   │  ShellMessage::{CreateVirtualPanel, ...}               - list/migrate windows & workspaces
   ▼                                                        - live reposition (keyword monitor)
2. DesktopOverlay + PcInput (desktops.rs)                7. WlxClient (continuous Wayland loop)
   - input_focus: PhysicalScreen                            - OutputChangeEvent hotplug
   - interactive pointer & keyboard routing                 - live global origin/extent -> regions
   │  Grab release -> layout_changed()                   8. Direct Capture (ext-image-copy-capture-v1)
   ▼                                                        - bypasses XDPH & picker completely
3. 3D-to-2D Layout Solver (new module)                      - feeds GStreamer appsrc -> VA/NVENC
   - physical monitors fixed as anchors                  9. desktop_link.h
   - snaps virtual outputs flush (0 gap)                    - dynamic regions[surface] updates
   │  ShellMessage::SyncPanelLayout                         - video_shard::desktop_streams + surface
   └──────────────────────────────────────────────────────► (same V4L2 NV12 DMA-buf transport)
```

### 4.1 Surface IDs & Wire Protocol (`wayvr/wayvr-ipc/src/frame.rs`)

WiVRn's stream multiplexing already supports arbitrary `u32` surface IDs:
- [`wivrn/server/driver/desktop_link.h`](../wivrn/server/driver/desktop_link.h) indexes `regions` and `videos` by `uint32_t` surface ID and sends shards on `to_headset::video_shard::desktop_streams + surface` (`0x100 + surface`).
- [`wivrn/client/linux/desktop_receiver.h`](../wivrn/client/linux/desktop_receiver.h) stores streams in `std::map<uint32_t, std::unique_ptr<desktop_stream>>`.

**Surface ID Space:**
- `0..N`: Physical monitors (as discovered by [`desktop/src/capture.rs`](../desktop/src/capture.rs)).
- `VIRTUAL_SURFACE_BASE: u32 = 1 << 31` (`0x8000_0000`): virtual panel IDs allocate monotonically per connection as `VIRTUAL_SURFACE_BASE + slot` (`slot <= 0x7FFF_FEFF`, keeping `surface <= 0xFFFF_FEFF` so `desktop_streams (0x100) + surface` never overflows `u32`). Surface IDs are not reused within a connection.

**Protocol Updates in [`wayvr/wayvr-ipc/src/frame.rs`](../wayvr/wayvr-ipc/src/frame.rs):**
Add structured virtual-panel messages in [`wayvr/wayvr-ipc/src/frame.rs`](../wayvr/wayvr-ipc/src/frame.rs) (there is no XDG `ScreenCast` window-share fallback; virtual panels require virtual outputs, and the headset hides or disables the Virtual panels UI when `VirtualCapabilities.supported == false`):

```rust
pub const VIRTUAL_SURFACE_BASE: u32 = 1 << 31;

pub struct VirtualCapabilities {
    pub supported: bool,
    pub compositor: String,      // e.g. "hyprland"
    pub reason: Option<String>,  // shown in UI when Hyprland is detected but e.g. socket/protocol is missing
    pub max_panels: u32,         // default 3
}

pub enum VirtualTargetKind {
    Blank { width: u32, height: u32 },
    Window { address: String, title: String, app_id: String },
    Workspace { id: i32, name: String },
}

pub struct VirtualPanelInfo {
    pub surface: u32,
    pub output_name: String,     // e.g. "FRAME-V1"
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub logical_pos: [i32; 2],
    pub logical_size: [i32; 2],
    pub scale: f32,
}

pub struct PanelLayoutTarget {
    pub surface: u32,
    /// Target top-left position in the compositor's 2D integer logical pixel space.
    pub x: i32,
    pub y: i32,
}
```

- **`HostMessage` additions:**
  - `VirtualState { capabilities: VirtualCapabilities, panels: Vec<VirtualPanelInfo>, physical_monitors: Vec<PhysicalMonitorInfo> }` — authoritative snapshot broadcast on connect and after every virtual-panel create, close, move, or host-driven teardown so the headset reconciles cleanly without per-request correlation IDs.
  - `VirtualTargets { windows: Vec<...>, workspaces: Vec<...> }`
- **`ShellMessage` additions:**
  - `RefreshVirtualTargets`
  - `CreateVirtualPanel { source: VirtualTargetKind, size: [u32; 2] }` — atomic from the user's perspective: if migration or capture setup fails (for example, a listed window closed before selection), the host rolls back `FRAME-V{k}`, emits `HostMessage::Error` (shown as a VR toast), and broadcasts `HostMessage::VirtualState`.
  - `CloseVirtualPanel { surface: u32 }` — idempotent (closing an unknown or already-removed surface is a no-op).
  - `SyncPanelLayout { placements: Vec<PanelLayoutTarget> }`

**Mixed-version behavior:** `HostMessage` and `ShellMessage` are JSON ([`wayvr/wayvr-ipc/src/ipc.rs`](../wayvr/wayvr-ipc/src/ipc.rs)); unknown enum variants are logged and ignored ([`wayvr/wayvr/src/frame_host.rs`](../wayvr/wayvr/src/frame_host.rs)) without dropping the connection. If an older host connects to a newer headset, no `VirtualState` arrives, so the headset keeps the virtual panels UI hidden while VR streaming and physical monitors continue working normally. Keep matching builds for full virtual-panel functionality.

### 4.2 C ABI, WiVRn Server & Headset Decoder (`frame_abi.h`, `desktop_link.h`, `desktop_receiver.cpp`)

#### Live `regions` Updates
In [`wivrn/server/driver/desktop_link.h`](../wivrn/server/driver/desktop_link.h) (lines 92–133), `regions[surface.id]` is currently only written when `frame_host_callbacks::surface` fires, and `desktop_receiver.cpp` forwards every `desktop_surface` packet to WayVR.
- When a virtual panel is added, moved in 2D, resized, or removed, the global desktop bounding box (`origin` and `extent` across all active physical + virtual outputs) changes — meaning **every** monitor's normalized `region: [f32; 4]` changes!
- Calling `output.surface(...)` with updated `region` and unchanged `width`/`height`/`codec` already hits [`wivrn/client/linux/desktop_receiver.cpp`](../wivrn/client/linux/desktop_receiver.cpp) lines 55–73 without recreating the V4L2 decoder, **or** we can add a lightweight `surface_region(uint32_t id, const float region[4])` callback in [`wayvr/frame-abi/include/frame_abi.h`](../wayvr/frame-abi/include/frame_abi.h) + [`wivrn/server/driver/frame_host.h`](../wivrn/server/driver/frame_host.h) to update `regions[id]` in `desktop_link.h` with zero control-plane noise to the headset. A dedicated `surface_region` callback is cleaner and avoids spamming `to_headset::desktop_surface` during layout drags.

#### Hardware Encode/Decode Concurrency Budget
Every active desktop stream allocates significant resources:
- **PC host ([`desktop/src/capture.rs`](../desktop/src/capture.rs)):** One GStreamer VA-API (`vah264enc`/`vah265enc`) or NVENC (`nvh264enc`/`nvh265enc`) session per output.
- **Headset ([`wivrn/client/decoder/v4l2/v4l2_decoder.cpp`](../wivrn/client/decoder/v4l2/v4l2_decoder.cpp)):** One V4L2 `iris` hardware decoder instance per stream, allocating `output_buffer_count = 8` buffers (each `>= max(w*h, 4 MiB)`) and `capture_buffer_count = 12` linear NV12 DMA-bufs (`import_capture_buffer`).
- **Concurrency hazards already observed in code:** [`wivrn/client/linux/desktop_receiver.cpp`](../wivrn/client/linux/desktop_receiver.cpp) (lines 216–225) explicitly documents that the Qualcomm `iris` V4L2 driver can stall or enter an error state when several decoder sessions initialize concurrently.
- **Mitigations:**
  1. Enforce a configurable cap (default `max_panels = 3` virtual outputs in addition to physical monitors). Hidden virtual panels keep their paused V4L2 decoder alive (avoiding `iris` re-init stalls on unhide) and therefore still count toward `max_panels`; attempting to add past the cap disables the UI action / returns a clear toast.
  2. Stagger new `make_stream` / decoder initializations or serialize V4L2 stream creation behind a short mutex/delay in `desktop_receiver` if needed.
  3. Keep the visibility-based encode pause (`capture_enabled` probe in [`desktop/src/capture.rs`](../desktop/src/capture.rs) around line 389) working for virtual panels so hidden panels consume zero encode/Wi-Fi/decode bandwidth without modifying host outputs or 2D layout.

### 4.3 PC Host — Compositor Control Backend (`desktop/src/`)

> **v1 Scope Note:** Virtual panels are currently implemented **only for Hyprland**, with **no XDG `ScreenCast` portal fallback** (since the interactive, 60 Hz, edge-crossing workflow cannot function without real virtual outputs). On unsupported compositors, the host reports `VirtualCapabilities.supported = false`.

Introduce a clean `CompositorBackend` abstraction inside `desktop/src/` (`desktop/src/compositor/mod.rs`) so Hyprland-specific IPC stays isolated from capture and layout logic:

```rust
pub trait CompositorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_monitors(&self) -> anyhow::Result<Vec<MonitorGeometry>>;
    fn list_windows(&self) -> anyhow::Result<Vec<WindowInfo>>;
    fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceInfo>>;
    fn create_headless_output(&self, name: &str) -> anyhow::Result<()>;
    fn configure_output(&self, name: &str, mode: (u32, u32, u32), pos: (i32, i32), scale: f32) -> anyhow::Result<()>;
    fn remove_output(&self, name: &str) -> anyhow::Result<()>;
    fn move_window_to_output(&self, address: &str, output: &str, workspace_name: &str) -> anyhow::Result<()>;
    fn move_workspace_to_output(&self, workspace_id: i32, output: &str) -> anyhow::Result<()>;
}
```

**Hyprland implementation details:**
- Talk directly over Unix sockets (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`) rather than spawning `hyprctl` subprocesses in hot paths (zero fork overhead; faster and cleaner error handling).
- Listen on `.socket2.sock` for live `closewindow`, `openmonitorv2`, `monitorremoved`, and `workspace` events so if the user closes a popped-out app manually on the PC, a `Window` panel auto-closes cleanly while `Workspace`/`Blank` panels remain open.
- **Output ownership & orphan sweep:** Because `frame_host` serves one headset connection per PC host ([`desktop/src/lib.rs`](../desktop/src/lib.rs)), `FRAME-V1`, `FRAME-V2`, ... are exclusively owned by Frame Shell. On `frame_host_start` (prior to physical monitor capture) and `frame_host_stop`, evacuate any surviving windows off any `FRAME-V*` output onto a physical monitor before calling `output remove`.
- **Panel-type workspace ownership & restoration rules:**

| Panel Kind | Create & Workspace Setup | Duplicate Target | Close / Disconnect / App Exit |
| --- | --- | --- | --- |
| **`Blank`** | Create `FRAME-V{k}` and bind dedicated named workspace `name:frame-v{k}` via `moveworkspacetomonitor`. | N/A (each creates a new output up to `max_panels`). | Stays open if empty. On close/disconnect, move any windows placed onto `name:frame-v{k}` to the primary remaining physical monitor, then `output remove FRAME-V{k}`. |
| **`Window`** | Record the window's original workspace/monitor; create `FRAME-V{k}` + `name:frame-v{k}`; migrate the window via `movetoworkspacesilent`. | If that window is already owned by an active virtual panel, unhide/focus that panel instead of creating a second one. | Auto-closes if the tracked window exits (or is manually moved off `FRAME-V{k}`), evacuating any extra windows moved onto `name:frame-v{k}` first. On close/disconnect, return the window to its recorded workspace (or recorded monitor if the workspace vanished, fallback: primary physical monitor) before `output remove`. |
| **`Workspace`** | Record the workspace's original physical monitor; create `FRAME-V{k}` and move existing workspace `<id>` onto it via `moveworkspacetomonitor` (no `name:frame-v{k}` created). | If that workspace is already on an active virtual panel, unhide/focus that panel instead of creating a second one. | Stays open if empty. On close/disconnect, `moveworkspacetomonitor <id> <return_monitor>` (fallback: primary physical monitor if the original monitor was unplugged) before `output remove`. |

- **Output positioning:** Use `keyword monitor "<name>,<w>x<h>@<hz>,<x>x<y>,<scale>"` (and optionally `zwlr_output_manager_v1` from `wayland-protocols-wlr` 0.3.12, already present in [`desktop/Cargo.lock`](../desktop/Cargo.lock), as a compositor-neutral positioning/mode mechanism shared with Sway/niri).

#### Requirements to Support Additional Compositors
Any new compositor backend (Sway, niri, KWin, etc.) must satisfy five requirements before enabling `VirtualCapabilities.supported = true`:
1. **Dynamic virtual/headless outputs:** Programmatically create, resize/set mode, and destroy virtual `wl_output`s without user dialogs or restarting the compositor (e.g., Sway `create_output`, KWin fake/virtual output API).
2. **Live 2D layout reconfiguration:** Reposition virtual outputs in the desktop coordinate space (including negative coordinates and mixed DPI logical sizes) without interrupting active captures — either via `zwlr_output_manager_v1` or compositor IPC/D-Bus.
3. **Window & workspace introspection + silent migration:** List open windows and workspaces, subscribe to window-close events (for `Window`-panel auto-teardown), and move windows/workspaces onto and off virtual outputs without stealing focus from physical monitors.
4. **Picker-free, non-blocking capture:** Advertise `ext-image-copy-capture-v1` (`ext_output_image_capture_source_manager_v1`) or provide an equivalent per-output capture API that does not block existing streams or prompt the user.
5. **Backend integration:** Implement `CompositorBackend` and register compositor detection in `desktop/src/compositor/mod.rs` (plus any sandbox/IPC socket permissions needed in [`flatpak/io.github.konsti219.FrameShell.yml.in`](../flatpak/io.github.konsti219.FrameShell.yml.in)).

### 4.4 PC Host — Dynamic Output Tracking & Direct Capture (`desktop/src/capture.rs`, `wayvr/wlx-capture/`)

#### Continuous Output Tracking & Physical-Capture Boundary
Today, [`desktop/src/capture.rs`](../desktop/src/capture.rs) (lines 94–112) creates `WlxClient::new()`, reads `outputs`, `origin`, and `extent` once, and drops `wayland`.
- Keep a persistent `WlxClient` event loop running on a host thread (using `dispatch_pending()` and `iter_events()` already implemented in [`wayvr/wlx-capture/src/wayland.rs`](../wayvr/wlx-capture/src/wayland.rs)).
- Whenever `OutputChangeEvent::{Create, Logical, Physical, Destroy}` fires:
  - Recompute `origin` and `extent` across all current `wl_output`/`zxdg_output_v1` entries.
  - Recompute normalized `region: [f32; 4]` for **every** active stream (both physical PipeWire monitors and virtual Wayland panels) and push updated regions into `desktop_link.h`.
- **Boundary for physical monitor capture:**
  - `capture::run` keeps using the XDG `ScreenCast` portal only for physical monitors discovered at session start; hot-plugging a physical monitor updates `regions` live without automatically popping up a blocking mid-session portal prompt.
  - Do **not** register `FRAME-V*` headless outputs into `capture::run` (filter out `FRAME-V*`), **and** in the portal stream matching block ([`desktop/src/capture.rs`](../desktop/src/capture.rs) lines 223–233), explicitly reject `selected.mapping_id` starting with `FRAME-V` before falling back to `.or(expected)` so a virtual output can never be misbound as a physical monitor.

#### Direct Virtual Output Capture (`ext-image-copy-capture-v1`)
- Why not `zwlr_export_dmabuf_manager_v1`? Hyprland dropped `zwlr_export_dmabuf_manager_v1` when moving off `wlroots`.
- Why not `xdg-desktop-portal-hyprland`? XDPH blocks all streams whenever its picker runs and doesn't expose programmatic headless-output selection without a restore-token prompt.
- **Solution:** Capture each `FRAME-V*` `WlOutput` directly using **`ext-image-copy-capture-v1`** (paired with `ext-output-image-capture-source-manager-v1`, both available in `wayland-protocols` 0.32.13, already in [`desktop/Cargo.lock`](../desktop/Cargo.lock)).
- Feed captured frames into a GStreamer `appsrc` driving the **exact same encoder tail** currently in `stream_codec` ([`desktop/src/capture.rs`](../desktop/src/capture.rs) line 308): `videorate drop-only=true` → `va{codec}enc`/`nv{codec}enc` → `h264parse`/`h265parse` → `appsink`.
- **Two-stage rollout for capture buffers:**
  1. **Stage A (Phase 2 dev milestone):** Capture into `wl_shm` buffers (`BGRA`/`BGRx`) → push to `appsrc` (`video/x-raw,format=BGRx`) → `vapostproc`/`videoconvert` → `NV12`. Fast to validate end-to-end.
  2. **Stage B (Ship target):** Allocate GBM / `linux-dmabuf` buffers (`zwp_linux_dmabuf_v1`), attach them to `ext_image_copy_capture_frame_v1`, and push `GstMemory` DMA-buf buffers (`video/x-raw(memory:DMABuf)`) straight into `vapostproc` for zero CPU readback under multi-panel + VR workloads.
- **Cursor handling:** Request `paint_cursors = 1` when creating the `ext_image_copy_capture_session_v1` (just as `embed_mouse: true` is used for physical monitors) so the cursor renders inside the panel when hovering it, and does not freeze on screen when the cursor leaves.

### 4.5 Headset — UI Entry Points, Interactive Overlays & 3D-to-2D Layout Solver

#### Keyboard Bar & Dashboard UI (`keyboard.xml`, `overlay_list.rs`, `button.rs`, `tab/wivrn.rs`)
- **Keyboard hamburger menu (`menu_burger` in [`wayvr/wayvr/src/assets/gui/keyboard.xml`](../wayvr/wayvr/src/assets/gui/keyboard.xml)):** Add an **Add virtual panel** cell alongside `BAR.ADD_MIRROR` and `BAR.ADD_PASSTHRU` (skipped when no connected PC reports `VirtualCapabilities.supported == true`), dispatched via [`wayvr/wayvr/src/gui/panel/button.rs`](../wayvr/wayvr/src/gui/panel/button.rs) to open an in-shell popup/context picker for target PC, blank presets, open windows, and workspaces.
- **PC group button (`HostGroup` / `menu_host` in [`wayvr/wayvr/src/assets/gui/keyboard.xml`](../wayvr/wayvr/src/assets/gui/keyboard.xml) and [`wayvr/wayvr/src/gui/panel/overlay_list.rs`](../wayvr/wayvr/src/gui/panel/overlay_list.rs)):** Add **Add virtual panel…** per connected PC in `menu_host`, and display active virtual panels as buttons in that PC's `HostGroup` beside its physical `Screen` buttons.
- **Virtual panel button menu (`menu_virtual_screen`):** Left-clicking a virtual panel button toggles visibility (`::OverlayToggle`); right-clicking offers visibility toggle, keep-visible-when-hidden, reset position, and **Close virtual panel** (`::FrameShellVirtualClose` → `ShellMessage::CloseVirtualPanel`).
- **Dashboard → PC (WiVRn) tab ([`wayvr/dash-frontend/src/tab/wivrn.rs`](../wayvr/dash-frontend/src/tab/wivrn.rs), [`wayvr/dash-frontend/assets/gui/tab/wivrn.xml`](../wayvr/dash-frontend/assets/gui/tab/wivrn.xml)):** Mirrors active panel management, full scrollable window/workspace selection, and hosts the **Auto-sync VR layout to desktop** toggle.

#### Overlay Lifecycle, Hiding & Input (`wayvr/wayvr/src/frame_shell/desktops.rs`)
- Virtual panels (`surface >= VIRTUAL_SURFACE_BASE`) are **transient**:
  - Do **not** register them in `frame_hosts::monitor()` / `~/.config/wayvr/frame-shell-screens` (otherwise disconnected virtual panels would spawn dead placeholders on next boot).
  - Instead, identify them by `"pc-virtual-<source>-<surface> <output_name>"` (or similar stable non-persisted key) and remove their `DesktopOverlay` via `OverlayTask::DropImmediate` + clean up `app.session.config.attribs` on close/disconnect.
- **Hiding & overlay scale separation:**
  - Hiding a virtual panel (or switching overlay sets) sends `DesktopVisibility { visible: false }`, which pauses host capture/encoding only; the compositor output and 2D layout remain intact. (**Keep visible when hidden** simply keeps the overlay active and streaming across set switches.)
  - Scaling a panel in WayVR edit mode only resizes its 3D quad in meters and does **not** change Hyprland's output mode or logical pixel dimensions.
- Virtual panels keep the standard `input_focus: Some(InputFocus::PhysicalScreen)` in `desktop_overlay(...)` ([`desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs) line 343).
- Because `PcInput::mouse_move` already encodes `(source, surface)` on every `kind == 2` mouse move ([`desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs) line 271) and `desktop_link.h` maps `event.x`/`event.y` through `regions[event.surface]`, VR laser hover, click, scroll, and virtual keyboard input follow the exact same path as physical monitors once `regions[surface]` is populated.

#### 3D-to-2D Layout Solver Algorithm

**Goal:** Whenever virtual panels are spawned, moved (grab release), or closed in 3D space, derive a valid, **gap-free, overlap-free** 2D integer-logical-pixel arrangement (`PanelLayoutTarget { surface, x, y }`) for the PC host where **physical monitors never move** and virtual outputs snap cleanly to the edges that match their 3D placement.

**Inputs (for one connected PC `source`):**
1. **Physical anchors:** List of physical monitors `{ id, logical_pos: (x, y), logical_size: (w, h), overlay_transform: Option<Affine3A> }`. These coordinates `(x, y, w, h)` are **immutable** constraints. Deduplicate mirrored/overlapping physical monitor rectangles when picking the primary anchor (prefer the monitor at `(0, 0)` or largest area) so mirrored displays do not confuse the projection basis.
2. **Virtual panels:** All active virtual panels `{ surface, logical_size: (w, h), overlay_transform: Affine3A }` (including temporarily hidden ones, using their stored `state.transform` so hiding never disrupts 2D edges).
3. **Anchor reference frame (head-movement invariant):** Anchor the 2D projection basis to the **primary physical monitor overlay's 3D pose** `(right = +X_local, up = +Y_local, center = P_0)` so turning or tilting your head never changes the 2D layout. Only if no physical monitor overlay is active in WayVR, fall back to the viewer's forward/right frame captured once at panel spawn time.

**Algorithm Steps:**
1. **3D → Desired 2D Center Projection:**
   - Using the primary physical monitor's tangent/cylindrical frame centered at `P_0` (using radius from `P_0` to the viewer origin or primary screen forward plane):
     - Horizontal displacement/azimuth right of `P_0` maps to `target_cx = primary_cx + dx_px`.
     - Vertical displacement/elevation above `P_0` maps to `target_cy = primary_cy - dy_px` (Wayland/Hyprland Y-axis points **down**, so **above** is negative Y).
     - This naturally converts curved 3D arcs around a desk into flat side-by-side 2D monitors, and panels placed above/below into negative/positive Y rows.
2. **Deterministic Edge-Snapping Packer & Fallback:**
   - Initialize `placed = physical_monitors.clone()`.
   - Sort virtual panels deterministically by distance from `P_0` (tie-break by `surface` ID).
   - For each virtual panel `V` with size `(vw, vh)` and desired center `(tx, ty)`:
     - Enumerate candidate positions flush against all 4 edges of every already-placed rectangle `A = (ax, ay, aw, ah)` in `placed`:
       - **Above `A`:** `y = ay - vh`, `x = clamp(tx - vw/2, ax - vw + min_overlap, ax + aw - min_overlap)`
       - **Below `A`:** `y = ay + ah`, `x = clamp(tx - vw/2, ax - vw + min_overlap, ax + aw - min_overlap)`
       - **Left of `A`:** `x = ax - vw`, `y = clamp(ty - vh/2, ay - vh + min_overlap, ay + ah - min_overlap)`
       - **Right of `A`:** `x = ax + aw`, `y = clamp(ty - vh/2, ay - vh + min_overlap, ay + ah - min_overlap)`
       *(Note: `min_overlap = 48.min(vw).min(vh).min(aw).min(ah)` guarantees a usable mouse-crossing bridge without exceeding small monitor dimensions; optionally snap `x`/`y` within 32 px to `ax`/`ay` or center alignment.)*
     - Filter out any candidate rectangle overlapping any rectangle in `placed` (strict interior intersection test: `r1.x < r2.x2 && r1.x2 > r2.x && r1.y < r2.y2 && r1.y2 > r2.y`).
     - Score surviving candidates by Euclidean distance between candidate center `(cx, cy)` and `(tx, ty)`, applying **hysteresis** (subtract a sticky bonus, e.g., 64 logical px, when the candidate touches the same `(parent_id, edge_side)` as the panel's previously committed placement so near-diagonal placements don't flip-flop on minor adjustments). Tie-break deterministically by `(parent_id, edge_side, x, y)`.
     - **Guaranteed fallback:** If every candidate collides (e.g., crowded inner pockets), place `V` flush against the rightmost `max_x` edge of `placed`'s bounding box aligned vertically with an existing monitor row, which is mathematically guaranteed never to overlap.
     - Commit the winning `(x, y)` to `placed`.
3. **Debounce & Auto-Sync Toggle Behavior:**
   - Trigger the solver ~300–500 ms after grab release (alongside `layout_changed()` in [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs)) and on virtual panel add/close.
   - Skip sending `SyncPanelLayout` if the resulting `(x, y)` coordinates for all virtual panels match the last committed layout.
   - **Auto-sync toggle:** When **Auto-sync VR layout to desktop** is **off**, new virtual outputs simply append flush to the right edge of existing outputs and grab-release moves are ignored; re-enabling it immediately runs the solver once to sync the current 3D arrangement.

**Concrete Example (Virtual panel above the main screen):**
- Physical monitor `DP-1` is `2560×1440` at `(0, 0)`.
- User spawns `FRAME-V1` (`1280×1440` or `1920×1080`) with an application window and drags it above `DP-1` in VR.
- Solver computes `Δθ ≈ 0`, `Δφ > 0` → `target_cy < 0` → best non-overlapping edge contact is **Above `DP-1`**: `y = 0 - 1080 = -1080`, `x = 320` (centered above `2560` width, or `0` if snapped).
- Host issues `keyword monitor "FRAME-V1,1920x1080@60,320x-1080,1"`.
- Moving the physical mouse straight up off the top edge of `DP-1` enters `FRAME-V1` cleanly.

### 4.6 Packaging & Sandbox (`nix/host-module.nix`, `flatpak/io.github.konsti219.FrameShell.yml.in`)

- **NixOS / native (`nix/host-module.nix`):** `frame-shell.service` runs in the user's graphical systemd session (`after = [ "graphical-session.target" ]`) and inherits `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and `HYPRLAND_INSTANCE_SIGNATURE` (when Hyprland exports them to systemd user environment). No sandbox changes needed.
- **Flatpak (`flatpak/io.github.konsti219.FrameShell.yml.in`):**
  - Already has `--socket=wayland` (line 19), which covers `WlxClient`, `ext-image-copy-capture-v1`, and `zwlr_output_manager_v1`.
  - Add `--filesystem=xdg-run/hypr` (or `--filesystem=xdg-run/hypr:rw`) to `finish-args` so the Flatpak server can connect to Hyprland's `.socket.sock` and `.socket2.sock` without needing `flatpak-spawn --host`.

---

## 5. Step-by-Step Implementation Guide

Follow these phases sequentially. Every phase ends with concrete verification steps using the project's `just` workflows from [`DEVELOPMENT.md`](../DEVELOPMENT.md).

### Phase 0: Host Feasibility Spike (1–2 hours, Go/No-Go Check)

**Goal:** Validate Hyprland's virtual output behavior, live `ext-image-copy-capture-v1` streaming during output repositioning, and `/dev/uinput` `EV_ABS` behavior on negative-origin, mixed-scale, and L-shaped layouts before writing production code.

1. **Test headless output creation & above-monitor positioning on Hyprland:**
   ```sh
   hyprctl output create headless FRAME-V1
   hyprctl monitors -j   # note your primary monitor name, pos, size, scale
   # Example: if primary is 1920x1080 at 0x0 with scale 1:
   hyprctl keyword monitor "FRAME-V1,1920x1080@60,0x-1080,1"
   ```
   - Open a test window, move it to `FRAME-V1` (`hyprctl dispatch movetoworkspacesilent 99,activewindow && hyprctl dispatch moveworkspacetomonitor 99 FRAME-V1`), and move your physical mouse **up** across the top edge of your primary screen. Verify smooth edge crossing and keyboard focus.
2. **Verify Wayland capture protocol & live headless capture + reposition:**
   ```sh
   wayland-info | grep -E "ext_image_copy_capture|ext_output_image_capture|zwlr_output_manager"
   ```
   - Confirm `ext_image_copy_capture_manager_v1` and `ext_output_image_capture_source_manager_v1` are advertised.
   - **Real capture check:** Run a small prototype/CLI test capturing `FRAME-V1` via `ext-image-copy-capture-v1` (confirming frames arrive at 60 Hz even when `FRAME-V1` is the only output showing an updating window), then run `hyprctl keyword monitor "FRAME-V1,1920x1080@60,-1920x0,1"` **while capturing**. Record whether the capture session stays alive or emits `stopped` (so Phase 2 handles session recreation cleanly if needed).
3. **Verify `/dev/uinput` `EV_ABS` behavior on L-shaped, negative-origin & mixed-scale layouts:**
   - While `FRAME-V1` is at `0x-1080` (and at an offset L-shaped position like `320x-1080` over a `2560×1440` or scaled primary), send synthetic `EV_ABS` values (`0..65535`) via `Frame Shell Mouse` (`wivrn_uinput.cpp`).
   - **Check:** Does Hyprland map `EV_ABS` `[0..65535]` over the global bounding box `[origin .. origin + extent]` accurately across negative origins, mixed scales, and empty L-shaped corners?
   - **Contingency:** If Hyprland mis-maps `EV_ABS`, verify **`zwlr_virtual_pointer_v1`** (`motion_absolute` per `wl_output`) on the same negative/mixed-scale layout as a drop-in desktop pointer injection replacement before proceeding past Phase 1.
4. **Clean up spike:**
   ```sh
   hyprctl output remove FRAME-V1
   ```

---

### Phase 1: Dynamic Output Tracking, Live Regions & Multi-Surface Support

**Goal:** Refactor the host + ABI + WayVR surface lifecycle from "static monitors at startup" to dynamic outputs with live `region` updates and multiple concurrent virtual surfaces.

**Files to touch:**
- [`wayvr/frame-abi/include/frame_abi.h`](../wayvr/frame-abi/include/frame_abi.h)
- [`wivrn/server/driver/frame_host.h`](../wivrn/server/driver/frame_host.h)
- [`wivrn/server/driver/desktop_link.h`](../wivrn/server/driver/desktop_link.h)
- [`wayvr/wayvr-ipc/src/frame.rs`](../wayvr/wayvr-ipc/src/frame.rs)
- [`desktop/src/lib.rs`](../desktop/src/lib.rs)
- [`desktop/src/capture.rs`](../desktop/src/capture.rs)
- [`wayvr/wayvr/src/frame_shell/mod.rs`](../wayvr/wayvr/src/frame_shell/mod.rs)
- [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs)

**Tasks:**
1. Define `VIRTUAL_SURFACE_BASE: u32 = 1 << 31` in [`wayvr/wayvr-ipc/src/frame.rs`](../wayvr/wayvr-ipc/src/frame.rs) (`surface <= 0xFFFF_FEFF`) and add the `HostMessage`/`ShellMessage` variants from §4.1.
2. Add `surface_region` callback (or update `regions` cleanly on surface updates) in [`wayvr/frame-abi/include/frame_abi.h`](../wayvr/frame-abi/include/frame_abi.h), [`wivrn/server/driver/frame_host.h`](../wivrn/server/driver/frame_host.h), [`wivrn/server/driver/desktop_link.h`](../wivrn/server/driver/desktop_link.h), and `Output` in [`desktop/src/lib.rs`](../desktop/src/lib.rs).
3. Refactor [`desktop/src/capture.rs`](../desktop/src/capture.rs) so `WlxClient` stays alive in a dedicated monitor-watch loop:
   - Maintain a `Shared` map of all active `wl_output` geometries (`id -> Monitor { name, logical_pos, logical_size, pixels }`).
   - Recompute `origin`/`extent` and emit updated `region`s for every active stream whenever `OutputChangeEvent` fires (hot-plugging a physical monitor updates `regions` only — never triggers a mid-session PipeWire portal prompt).
   - Ignore `FRAME-V*` outputs in `capture::run` and guard `selected.mapping_id` against `FRAME-V*` before `.or(expected)` ([`desktop/src/capture.rs`](../desktop/src/capture.rs) lines 223–233) so the portal can never bind a virtual output as a physical monitor.
4. Update [`wayvr/wayvr/src/frame_shell/mod.rs`](../wayvr/wayvr/src/frame_shell/mod.rs) and [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs):
   - Route `surface >= VIRTUAL_SURFACE_BASE` as interactive (`input_focus: Some(InputFocus::PhysicalScreen)`).
   - Add a transient retired-surface queue (e.g., `RETIRED_VIRTUALS`) so closed virtual surfaces drop immediately via `OverlayTask::DropImmediate` without registering in or touching `frame-shell-screens`.

**Verification:**
- `just build-host` and `just build-headset` compile cleanly.
- Run `just run`, manually run `hyprctl output create headless FRAME-V1` + `hyprctl keyword monitor "FRAME-V1,1920x1080@60,1920x0,1"`, and confirm host logs show `origin`/`extent` updating live and existing physical monitor VR laser coordinates stay calibrated to their physical screen rather than shifting!

---

### Phase 2: Picker-Free Capture via `ext-image-copy-capture-v1`

**Goal:** Stream any `FRAME-V*` `wl_output` into `Output::video` at 60 Hz using Wayland `ext-image-copy-capture-v1`, without invoking `xdg-desktop-portal-hyprland`.

**Files to touch:**
- [`desktop/Cargo.toml`](../desktop/Cargo.toml) (add `wayland-client`, `wayland-protocols` features if not re-exported through `wlx-capture`)
- New module `desktop/src/wayland_capture.rs` (or extension in [`wayvr/wlx-capture/src/`](../wayvr/wlx-capture/src/))
- [`desktop/src/capture.rs`](../desktop/src/capture.rs)

**Tasks:**
1. Bind `ext_output_image_capture_source_manager_v1` and `ext_image_copy_capture_manager_v1` from the Wayland registry.
2. Create an `ext_image_copy_capture_session_v1` for the target `WlOutput` with cursor painting enabled (`Options::PaintCursors`).
3. **Stage A (Dev milestone — SHM):** Read buffer constraints (`shm_format`, `buffer_size`), allocate a double-buffered `memfd` `wl_shm_pool`, attach damage, capture frames at up to 60 Hz (respecting `capture_enabled` visibility flag so hidden panels pause capture/encoding!), and push `gstreamer::Buffer`s into a GStreamer `appsrc` (`is-live=true`, `format=Time`, `do-timestamp=true`) feeding `videorate ! videoconvert ! video/x-raw,format=NV12 ! {encoder}`.
4. **Stage B (Ship target — zero-copy DMA-buf):** Read `dmabuf_device` / `dmabuf_format` constraints, allocate GBM/DRM-backed DMA-bufs, and push `gstreamer_allocators::DmaBufAllocator` buffers directly to `vapostproc` to eliminate CPU copies.
5. Handle `Stopped` / `SizeChanged` session events cleanly (recreate session on `Stopped` if output still exists; resize swaps `appsrc` caps or rebuilds pipeline; output removal exits worker thread).

**Verification:**
- Create `FRAME-V1` with an animated 60 FPS test application and stream it to the headset while physical monitors are also streaming.
- Check `metrics` via `$XDG_RUNTIME_DIR/frame-metrics.sock` (as documented in [`ARCHITECTURE.md`](../ARCHITECTURE.md)): verify ~60 FPS `captured`/`encoded`/`sent` on PC and `received`/`decoded`/`shown` on headset.
- Toggle panel visibility in WayVR and confirm encoding pauses (`"hidden; encoding stopped"`), without `FRAME-V1` disappearing from `hyprctl monitors -j`.
- Create a second virtual output (`FRAME-V2`) while `FRAME-V1` and physical monitors are streaming and verify **zero frame freeze** on existing streams.

---

### Phase 3: Hyprland Virtual Panel Lifecycle & Headset UI (Keyboard Bar + Dashboard)

**Goal:** Let users add, toggle, and close multiple virtual panels directly from the keyboard bar (`menu_burger`, `HostGroup` / `menu_host`) and the dashboard PC tab, with windows/workspaces migrated cleanly and restored on close/disconnect.

**Files to touch:**
- New module `desktop/src/compositor/mod.rs` and `desktop/src/compositor/hyprland.rs`
- [`desktop/src/lib.rs`](../desktop/src/lib.rs), [`desktop/src/capture.rs`](../desktop/src/capture.rs)
- [`wayvr/wayvr/src/frame_host.rs`](../wayvr/wayvr/src/frame_host.rs)
- [`wayvr/wayvr/src/frame_shell/pcs.rs`](../wayvr/wayvr/src/frame_shell/pcs.rs), [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs)
- [`wayvr/wayvr/src/assets/gui/keyboard.xml`](../wayvr/wayvr/src/assets/gui/keyboard.xml), [`wayvr/wayvr/src/gui/panel/overlay_list.rs`](../wayvr/wayvr/src/gui/panel/overlay_list.rs), [`wayvr/wayvr/src/gui/panel/button.rs`](../wayvr/wayvr/src/gui/panel/button.rs)
- [`wayvr/dash-frontend/src/tab/wivrn.rs`](../wayvr/dash-frontend/src/tab/wivrn.rs), [`wayvr/dash-frontend/assets/gui/tab/wivrn.xml`](../wayvr/dash-frontend/assets/gui/tab/wivrn.xml)
- [`flatpak/io.github.konsti219.FrameShell.yml.in`](../flatpak/io.github.konsti219.FrameShell.yml.in) (add `--filesystem=xdg-run/hypr`)

**Tasks:**
1. Implement Hyprland Unix socket client (`.socket.sock` for commands/JSON queries `j/monitors`, `j/workspaces`, `j/clients`; `.socket2.sock` for window/monitor events).
2. Implement `VirtualManager` on the PC host:
   - Tracks active virtual panels (`surface -> VirtualPanel { kind, output_name, workspace_id_or_name, tracked_window, return_target }`), capped at `max_panels` (default 3).
   - Startup/shutdown cleanup: run orphan sweep evacuating any windows off leftover `FRAME-V*` outputs before `output remove` (must run before `capture::run` starts).
   - Create flow: implement the 3-type create/duplicate/rollback rules from §4.3 (`Blank`, `Window`, `Workspace`), start `ext-image-copy-capture-v1` stream, and broadcast `HostMessage::VirtualState`. Roll back `FRAME-V{k}` and emit `HostMessage::Error` if any step fails.
   - Close/disconnect flow: implement the 3-type restoration rules from §4.3 (fallback to primary remaining physical monitor if the original monitor was unplugged) → `output remove FRAME-V{k}` → stop capture stream → emit `output.removed(surface)` + `HostMessage::VirtualState`. Ignore duplicate closes.
   - App-exit handling: listen on `.socket2.sock` for `closewindow` (and `movewindowv2`); when a `Window`-backed panel's target window exits (or is manually moved off `FRAME-V{k}`), automatically tear down that panel and its `FRAME-V{k}` output, while keeping `Workspace` and `Blank` panels open until explicitly closed.
3. Wire Frame Shell UI across both the keyboard bar and the dashboard PC tab:
   - **Keyboard bar ([`keyboard.xml`](../wayvr/wayvr/src/assets/gui/keyboard.xml), [`overlay_list.rs`](../wayvr/wayvr/src/gui/panel/overlay_list.rs), [`button.rs`](../wayvr/wayvr/src/gui/panel/button.rs)):**
     - Add **Add virtual panel** to `menu_burger` (skipped when no connected PC supports virtual outputs) and **Add virtual panel…** to `menu_host`.
     - Render active virtual panels in each PC's `HostGroup` alongside its physical screens, with a `menu_virtual_screen` right-click menu containing **Close virtual panel**.
     - Build the lightweight in-shell picker UI (blank resolution presets `1080p`, `1440p`, `1280×1440` portrait + live `VirtualTargets` window/workspace entries).
   - **Dashboard PC tab ([`wivrn.xml`](../wayvr/dash-frontend/assets/gui/tab/wivrn.xml), [`tab/wivrn.rs`](../wayvr/dash-frontend/src/tab/wivrn.rs)):**
     - Hide or disable the section when `VirtualCapabilities.supported == false` (showing `VirtualCapabilities.reason` if Hyprland is detected with missing prerequisites, else noting virtual panels require Hyprland).
     - Show the active panels list with title, resolution, and **Close** button, plus the full window/workspace/preset picker and `max_panels` status.
4. Keep an eye on `v4l2_decoder` startup stability ([`wivrn/client/linux/desktop_receiver.cpp`](../wivrn/client/linux/desktop_receiver.cpp)): verify adding 2–3 panels rapidly does not trigger `iris` init stalls; add a small creation lock/stagger in `desktop_receiver` if needed.

**Verification:**
- Run `just build-host`, `just headset`, `just run`.
- Add 2 virtual panels (one popped-out application window via the keyboard hamburger menu `menu_burger`, and one full workspace via `menu_host` or the PC tab) without touching the PC.
- Verify physical monitors never freeze, the popped-out window renders at 60 FPS even while switching workspaces on the physical monitor, and VR laser clicks/typing work on both virtual panels.
- Close the window-backed panel from the keyboard top-row context menu (`menu_virtual_screen`) and verify the window returns cleanly to the physical monitor and `hyprctl monitors -j` shows no `FRAME-V*` outputs left after closing all panels or disconnecting.

---

### Phase 4: Automatic 3D-to-2D Layout Solver & Live Sync

**Goal:** Moving a virtual panel anywhere around your physical monitors in VR automatically updates Hyprland's 2D monitor layout so physical mouse edge-crossing matches 3D space.

**Files to touch:**
- New pure-Rust solver module `wayvr/wayvr/src/frame_shell/layout_solver.rs` (unit-tested!)
- [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs), [`wayvr/wayvr/src/frame_shell/mod.rs`](../wayvr/wayvr/src/frame_shell/mod.rs)
- [`desktop/src/lib.rs`](../desktop/src/lib.rs), [`desktop/src/compositor/hyprland.rs`](../desktop/src/compositor/hyprland.rs)

**Tasks:**
1. Implement `layout_solver.rs` with strict geometric invariants:
   - Physical monitors stay fixed at their host-reported `(logical_pos, logical_size)` (mirrored physical monitors deduplicated).
   - Projection anchored to the primary physical monitor's 3D pose (head-rotation invariant).
   - Greedy edge-snapping packer + rightmost fallback guaranteeing:
     - **Zero overlap** between any two monitors.
     - **Non-trivial edge contact** (`>= min_overlap`, up to 48 logical px) between every virtual monitor and the connected graph of physical monitors.
     - Exact handling of negative coordinates (e.g., `0x-1080` above primary) and mixed HiDPI logical sizes (`logical_size = pixel_size / scale`).
     - True hysteresis (sticky edge preference) and deterministic tie-breaking.
2. Write comprehensive unit tests in `layout_solver.rs`:
   - Single physical monitor + virtual panel placed **above**, **below**, **left**, **right**, and **above-right**.
   - Dual physical monitors (side-by-side) and mirrored physical monitor test + 3 virtual panels in a curved arc above/around them.
   - Fractional scale test (e.g., 4K primary at scale 1.5 = `2560×1440` logical + `1920×1080` virtual at scale 1).
   - Near-diagonal hysteresis stability test + crowded fallback test.
   - Property assertions on every test case: zero rectangle intersection, every virtual monitor shares `>= min_overlap` edge overlap with at least one monitor reachable from physical monitors, physical coordinates unchanged.
3. Hook solver into [`wayvr/wayvr/src/frame_shell/desktops.rs`](../wayvr/wayvr/src/frame_shell/desktops.rs):
   - Run ~300–500 ms after grab release (`layout_changed()` / virtual panel add/close — do **not** run on hide/show), and send `ShellMessage::SyncPanelLayout` if any virtual `(x, y)` changed.
   - Add a dashboard toggle (`Auto-sync VR layout to desktop`, default **on**) with defined off/re-enable behavior (§4.5).
4. On the PC host, apply `SyncPanelLayout` via `compositor.configure_output(...)`, wait for `WlxClient` `OutputChangeEvent::Logical`, push updated `regions` to `desktop_link.h`, and broadcast `HostMessage::VirtualState`.

**Verification:**
- `cargo test --manifest-path wayvr/Cargo.toml` passes all `layout_solver` tests.
- End-to-end VR test:
  1. Pop out an application window onto `FRAME-V1`.
  2. Grab the panel in VR and place it **above** the main monitor, release, wait ~0.5 s.
  3. Confirm `hyprctl monitors -j` shows `FRAME-V1` at negative Y directly touching the top edge of the physical monitor.
  4. Move physical mouse **up**: cursor enters `FRAME-V1` cleanly; physical keyboard types into the window there (with `follow_mouse = 1` or click).
  5. Grab the panel in VR and move it to the **left** of the main monitor, release, wait ~0.5 s.
  6. Move physical mouse **left**: cursor enters `FRAME-V1` cleanly from the left edge; moving up no longer hits it.

---

### Phase 5 (Optional / Stretch): Gaze-Guided Cursor Warp & More Compositors

1. **Gaze-to-Warp ("MAGIC pointing"):**
   - Raycast `app.input_state.eye_gaze` ([`wayvr/wayvr/src/backend/openxr/input.rs`](../wayvr/wayvr/src/backend/openxr/input.rs)) against active `DesktopOverlay`s.
   - Bind an optional headset button or global PC hotkey (e.g., `Super+G` or mouse side button) to send `ShellMessage::FocusAndWarp { surface, uv }` → host moves cursor (`hyprctl dispatch movecursor` / `zwlr_virtual_pointer_v1`) + focuses window under cursor without needing big physical mouse swipes across panels.
2. **Additional Compositors:**
   - Implement `CompositorBackend` for **Sway** (`swaymsg` / `i3ipc` `create_output` + `zwlr_output_manager_v1`), **niri** (if/when dynamic outputs are supported), and **KDE KWin** (`org_kde_kwin_fake_output` / KWin D-Bus scripting).

---

## 6. Risks, Mitigations & Confirmed Decisions

| Risk | Impact | Mitigation |
| --- | --- | --- |
| **`EV_ABS` uinput mis-mapping on L-shaped or negative-origin desktops** | VR laser clicks land at wrong coordinates on virtual panels or physical screens. | Validate explicitly in **Phase 0**. If Hyprland fails to map `EV_ABS` over negative/L-shaped bounds, switch desktop pointer injection to `zwlr_virtual_pointer_v1` (which takes per-`wl_output` absolute coordinates). |
| **V4L2 `iris` decoder stalls or memory exhaustion** | Opening a 3rd/4th panel stalls an existing monitor or fails to decode. | Cap virtual panels (default 3), keep visibility-based encode/decode pause active, and serialize/stagger `make_stream` calls in [`desktop_receiver.cpp`](../wivrn/client/linux/desktop_receiver.cpp). |
| **Orphaned headless outputs on host crash** | User is left with invisible monitors eating their mouse after `wivrn-server` crashes. | Deterministic `FRAME-V*` naming + cleanup sweep in `frame_host_start` and `frame_host_stop` (`Drop`). |
| **Mixed DPI / fractional scaling** | Panel edge lengths or mouse speeds feel mismatched between 4K scaled monitor and 1080p virtual panel. | Compute all solver math in **logical** coordinates (`WlxOutput::logical_size`), and default new virtual panels to scale `1.0` (or match the primary monitor's scale). |
| **Flatpak missing Hyprland socket access** | Works in NixOS/`just run` dev server, fails in Flatpak release. | Add `--filesystem=xdg-run/hypr` in [`flatpak/io.github.konsti219.FrameShell.yml.in`](../flatpak/io.github.konsti219.FrameShell.yml.in) and surface a clear capability message if the socket isn't reachable. |

### Confirmed Product Decisions
1. **Closing a window-backed panel:** Closing the virtual panel returns the popped-out window to a physical monitor rather than closing the application.
2. **Quitting a popped-out app on the PC:** `Window`-backed panels automatically close and tear down their headless output when their app exits; `Workspace` and `Blank` panels stay open until explicitly closed.
3. **No XDG `ScreenCast` portal fallback:** Virtual panels require real virtual outputs — we intentionally do not provide a view-only XDG `ScreenCast` portal fallback. v1 supports **Hyprland only**; other compositors report `VirtualCapabilities.supported = false` until a backend meeting the requirements in §4.3 is implemented.

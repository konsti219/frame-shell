# Frame shell

WiVRn streaming with headset-local WayVR compositing: system UI, keyboard, native windows, raw PC desktops,
and one host-composited VR stream. The separate [Frame Monado](https://github.com/konsti219/frame-monado)
repository owns headset tracking, passthrough, floor estimation, compositor and timewarp.

## Work on it

```sh
git clone git+ssh://github.com_konsti219/konsti219/frame-shell.git
cd frame-shell
./build host
./build headset
./run-host
```

`./run-host` serves the development PC server separately from the system-installed WiVRn. It stops the installed
user unit while running, uses private state under `out/host/`, selects the development OpenXR runtime, and
restores the installed service/runtime on exit. Normally launched Steam VR games use it. All servers,
development and installed, share `${XDG_STATE_HOME:-~/.local/state}/frame-desktop/capture-token`.
The first connection may ask you to select your monitors; subsequent sessions save and reuse replacement tokens.
Do not copy tokens into separate test directories: portal restore tokens are single-use. An explicit
`FRAME_SHELL_CAPTURE_TOKEN` overrides the shared path; development runs migrate the previous `out/host/` tokens.

```sh
./deploy                         # headset shell/client only; Monado must already be running
./host app <command> <arguments>  # launch explicitly against the development runtime
./host ctl list-paired
./host stop                      # recover after a killed run-host
```

Nix supplies host dependencies; Podman supplies the ARM cross-compilation SDK. No NixOS rebuild is needed.
All sources and compilation stay on the PC. `FRAME_HEADSET` overrides the default SSH target `steamos@frame`.
For everyday installation, NixOS PCs import the flake's `nixosModules.host` and set
`services.frame-shell.enable`: the server with its host services as a user service (`frame-shell.service`), its
port, mDNS publishing and uinput (`inputUsers`). VR extras stay outside: `settings.openvr-compat-path` with the
flake's `xrizer` (the Frame's controller patches), `steamPackage` to start Steam games. The flake also builds the
PC-side WayVR IPC clients from this tree: `wayvrctl`, the Firefox media bridge and its extension (`media-bridge`,
`ytmusic-extension`). Patches for upstream packages are in `nix/`.

The headset's "Desktop" app (WayVR's launcher) is SteamOS's nested Plasma (`steamos-nested-desktop`, as in
SteamVR) in a WayVR window. It is registered in memory by the shell and uses WayVR's normal app launch path.
The Plasma handler passes the Wayland socket by absolute path, hides Steam's autostart with temporary runtime
configuration, and stops the nested session together. No extra desktop entry, helper script or configuration
is installed; pinned apps and autostart retain their normal behavior.
Plasma's apps select their own X11/Wayland backends; outer Gamescope settings are cleared so Steam can run
as a desktop client instead of forcing gamepad mode or a Wayland backend it does not support.
Windows OpenXR games under x86-64 Proton/FEX use frame-monado's x86-64 client and the native ARM64 service.
Build/deploy the current frame-monado first: its shared `openxr_frame.json` selects the matching architecture.
The shell launcher also exposes Monado's IPC socket inside Steam's game containers.
Its `VR_OVERRIDE` selects a bundled xrizer for OpenVR and Proton's VR initialization, using the same
revision and Frame controller patches as the PC package (`nix/xrizer.json`). Both ARM64 and x86-64
clients are built by `./build headset` and installed by `./deploy`; SteamVR's global runtime remains unchanged.
Steam's client runs as a WayVR app (X11 through a bundled `xwayland-satellite`, which SteamOS lacks); the launcher
clears the SteamOS session's Gamescope variables so it starts as the desktop client. Starting a game from the
dashboard or a pin starts Steam on WayVR's displays if it isn't running. Android games run through Lepton: the
shell's `PATH` holds `steamvr`, whose `path` gives Lepton a folder with frame-monado's Android client and a hard
link to Monado's socket (Lepton mounts it as the Android runtime), and `podman`, which uses the user's own session
bus and runtime folder when Steam runs in Plasma. `gamescope-0` links to WayVR's socket (KWin's in Plasma), the
Wayland display Lepton expects.

## Several PCs

The headset connects to every PC marked auto-connect (and any connected from the WiVRn tab), all with the same
server and protocol. Each streams its monitors; one presents VR. The others stand by: their sessions,
applications and monitors stay up, but they encode no VR video, get no tracking, and the headset holds no VR
decoders for them (`video_active`). The PC whose VR application started last presents; when that application
ends or the PC goes away, one still running an application takes over. Starting an application on a PC on
standby from the shell starts it there (`on <source> start_app`), which then switches. The keyboard's top row shows the headset's
own applications next to the WayVR button, then a group per PC: a button with its actions (connect, disconnect),
its screens, its pins and running applications. Running applications are the runtime's clients (libmonado, on the
headset and through each PC's host), with their Steam game's artwork when `/proc/<pid>/environ` has `SteamAppId`.
Clicking one shows it: its PC presents (`present <cookie>`, which holds until a VR application starts elsewhere)
and it becomes the runtime's primary client.

## Source map

| What | Edit here |
| --- | --- |
| WiVRn client, server and shared wire protocol | `wivrn/` |
| Embedded shell, watch, windows, audio, floor UI | `wayvr/wayvr/src/` |
| Dashboard UI and layouts | `wayvr/dash-frontend/src/`, `wayvr/dash-frontend/assets/gui/` |
| PC host services: monitor capture, WayVR IPC, notifications, metrics, games | `desktop/src/` |
| Shell ↔ host messages | `wayvr/wayvr-ipc/src/frame.rs` |
| C ABIs (headset shell, PC host) | `wayvr/frame-abi/include/frame_abi.h` |
| Build scripts | `build`, `scripts/` |
| Headset launcher and service | `headset/` |

These are ordinary tracked source directories. Edit, build, commit and push here; there are no authoritative
ignored clones or patch export step. Client and server share one `wivrn/common/wivrn_packets.h`. Both C ABIs
are defined once in `frame_abi.h`; Rust gets them through bindgen at build time.

`desktop/` builds `libframe_host.a`, which the server links and starts per headset connection. `capture.rs`
streams monitors; `ipc_proxy.rs` tunnels WayVR IPC clients such as the browser media bridge; `notifications.rs`
forwards VRCX and desktop notifications; `metrics.rs`, `system.rs` and `games.rs` cover the watch.
Host messages and commands travel through WiVRn's authenticated connection as opaque bytes.

`out/` contains ignored build products, caches and private pairing state. The build scripts
never copy source onto the headset. Headset outputs are under `out/headset/stage/frame-shell/`; the host runtime
manifest is generated at `out/host/openxr_wivrn-dev.json`.

See [ARCHITECTURE.md](ARCHITECTURE.md) for process ownership, ABI, wire messages and external interfaces.

## Monado dependency

The headset runtime is independently built/deployed from `frame-monado`'s `main` branch. `./build headset`
builds only the WiVRn client and WayVR; `./deploy` restarts only the shell and leaves Monado installed.

The PC server needs WiVRn's different Monado revision. That committed source is on `frame-monado`'s `wivrn`
branch, pinned in this repository's `flake.lock`. No runtime integration patches are applied during our builds.
To work on that dependency locally:

```sh
# In the frame-monado repository:
git worktree add ../frame-monado-host wivrn

# In frame-shell:
FRAME_HOST_MONADO=../frame-monado-host ./build host
```

Once the host runtime change is committed and pushed, update the pinned dependency with
`nix flake update host-monado`, rebuild, and commit the lock file.

## Upstream and checks

WiVRn and WayVR were imported with Git subtree, so their source is present in a normal clone. No submodule init
or private scratchpad checkout is needed. Import revisions and update commands are in [UPSTREAM.md](UPSTREAM.md).
Feature status is in [PLAN.md](PLAN.md).

Validate host changes with `./build host`, headset changes with `./build headset`, and protocol changes with
both. Runtime changes belong in frame-monado. Deployment checks dynamic dependencies before replacing binaries.
Headset logs are available with:

```sh
ssh steamos@frame 'journalctl --user -u frame-shell.service -u monado.service -n 100 --no-pager'
```

### Streaming over Wi-Fi

The headset streams over home Wi-Fi only; the Frame's USB dongle isn't used (it lost 3–4× more frames than a good
5 GHz channel; the integration is on the `dongle` branch). The WiVRn tab shows the Wi-Fi link's signal and rates.

Error correction (WiVRn tab, `fec_percent` in the headset's `client.json`, default 10) adds erasure-code
parity to every VR and desktop frame; it applies live and costs only its share of the bitrate. Over home Wi-Fi at
100 Mbit/s, 10% repairs about half the lost frames.

Retransmission (`retransmit` in `client.json`, default on): when a frame's packets go missing,
the headset asks for exactly those (or, for VR, everything after the last one received when the frame's end didn't
arrive), and the PC resends them from the last 8 frames it keeps per stream. A complete newer VR frame isn't held
back for an older one's resend: the encoder only references frames the headset confirmed, so it decodes on its own.
Desktop frames decode in order, so a later one waits up to 40 ms for the resend before falling back to a keyframe. Desktop
keyframes are only sent on request (and every 20 s).

A frame decoded after the headset picked the frame for its display slot is shown one slot late and the next one is
skipped: a jump while the picture moves (the WiVRn tab's "repeated frames"). The PC aims to have frames decoded the
99.5th percentile of its frames' delivery plus the frame margin (WiVRn tab, `frame_margin_ms`, default 1) before
that; a larger margin trades that much latency for fewer late frames. When a frame's packets stop before its end,
the headset asks for the rest after 3 ms instead of waiting for the next frame.

Speaker and microphone audio go over the stream socket (UDP) instead of the TCP control connection, whose resends
after a lost packet stall everything behind it (up to its ~200 ms timeout). Each chunk is sent again two chunks
(~6 ms) later and the receiver puts them back in order.

For testing, `simulated_loss_percent` and `simulated_loss_burst` in the headset's `client.json` drop that share of
incoming video packets in runs of about that many packets.

The WiVRn tab shows the Wi-Fi link (signal, rates) and the VR stream's freezes over 50 ms in the last 5 s.

### Defoveation in the runtime

With frame-monado's `XR_FRAME_foveated_projection`, the client copies the decoded (foveated) frame 1:1 and attaches
the per-axis foveation segments to the projection layer; Monado expands it in its distortion pass. That skips the
client's full-size pass (1.75 → 0.45 ms GPU) and a 3240² write and read per frame, for ~1 ms more in Monado's pass:
~0.5 W less in the spin test. Alpha streams, the stream GUI's dimming and the foveation tint use the old pass;
`runtime_defoveation: false` in `client.json` forces it.

### Eye expressions

The headset client runs Babble Frame Runtime's eye model (vendored in `wivrn/client/frame_eyes/babble/`, GPL-3.0;
the model is downloaded by `./build headset`) on the raw eye-camera frames from Valve's `dsp_service`, through
ExecuTorch (cross-built into the build container). It gives per-eye openness, widen and pupil dilation, calibrated
to the wearer after a few seconds and again each time the headset is put on; per-eye gaze comes from Valve's eye
tracker (`/dev/shm/eye-server.mmap`) with the shell's click calibration. The `from_headset::frame_eyes` packet
carries them over the stream socket, and the server sends them as VRCFT v2 avatar parameters
(`v2/EyeLeftX/Y`, `v2/EyeRightX/Y`, `v2/EyeX/Y`, `v2/EyeLidLeft/Right` with widen above 0.75, `v2/EyeLid`,
`v2/PupilDilation`, plus `EyeTrackingActive`) to VRChat's OSC port. VRChat's native eye tracking isn't sent.
The WiVRn tab's eye tracking line shows the update rate and whether it is still calibrating.

- Headset `client.json`: `eye_expressions` (default on), `eye_expressions_divisor` (one model update per that many
  camera frames, default 1). The cameras run at the rate Valve's eye service sets (15 fps while unworn).
- Server config: `eye-osc` (`host:port`, default `127.0.0.1:9000`; empty or `false` turns it off) and
  `eye-osc-prefix` (default `/FT`).

Server config `tracking-sample-ms` (default 6, upstream 3): the prediction step between the headset's tracking
samples of a device. Each sample is a runtime query and a packet; 6 ms halves them (~0.25 W on the headset in the
spin test), 12 ms saved nothing more.

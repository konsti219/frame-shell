# Frame shell development

WiVRn streaming with headset-local WayVR compositing: system UI, keyboard, native windows, raw PC desktops,
and one host-composited VR stream. The separate [Frame Monado](https://github.com/konsti219/frame-monado)
repository owns headset tracking, passthrough, floor estimation, compositor and timewarp.

## Work on it

```sh
git clone https://github.com/konsti219/frame-shell.git
cd frame-shell
just build-host
just build-headset
just run
```

`just` lists the recipes. `just run` serves the development PC server separately from the system-installed WiVRn. It stops the installed
user unit while running, uses private state under `out/host/`, selects the development OpenXR runtime, and
restores the installed service/runtime on exit. Normally launched Steam VR games use it. All servers,
development and installed, share `${XDG_STATE_HOME:-~/.local/state}/frame-desktop/capture-token`.
The first connection may ask you to select your monitors; subsequent sessions save and reuse replacement tokens.
Do not copy tokens into separate test directories: portal restore tokens are single-use. An explicit
`FRAME_SHELL_CAPTURE_TOKEN` overrides the shared path; development runs migrate the previous `out/host/` tokens.

```sh
just deploy                       # headset shell/client only; Monado must already be running
just headset                      # build-headset, then deploy
just all                          # also builds and deploys ../frame-monado (FRAME_MONADO), and builds the host
just app <command> <arguments>    # launch explicitly against the development runtime
just ctl list-paired
just stop                         # recover after a killed just run
```

Nix supplies host dependencies; Podman supplies the ARM cross-compilation SDK. No NixOS rebuild is needed.
All sources and compilation stay on the PC. `FRAME_HEADSET` overrides the default SSH target `steamos@frame.local`.
For everyday installation, NixOS PCs import the flake's `nixosModules.host` and set
`services.frame-shell.enable`: the server with its host services as a user service (`frame-shell.service`), its
port, mDNS publishing and uinput (`inputUsers`). VR extras stay outside: `settings.openvr-compat-path` with the
flake's `xrizer` (the Frame's controller patches), `steamPackage` to start Steam games.
`wayvrctl` is the PC-side WayVR IPC client from this tree; `just deploy` also installs it on the headset, where
`wayvrctl apps` lists what runs on the headset and on each PC (the keyboard's top row). Patches for upstream packages
are in `nix/`.

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
clients are built by `just build-headset` and installed by `just deploy`; SteamVR's global runtime remains unchanged.
The dashboard's "Battery / performance" menu shows the battery as SteamVR reads it (Valve's charger service in
`/run/deckardcharger`), the power draw from the board's power monitors, and switches between two performance modes:
"Streaming optimized" (GPU capped at 578 MHz) and "Standard" (uncapped, for native games). The mode is
`~/.config/frame-shell/performance-mode` (`streaming` or `standard`); the shell applies it at start and whenever
something lifts it (Steam sets the GPU level to auto when it starts), and `frame-shell.service` lifts the cap when the
shell stops. On battery the shell warns at 10, 5 and 1 %.

Steam's client runs as a WayVR app (X11 through a bundled `xwayland-satellite`, which SteamOS lacks); the launcher
clears the SteamOS session's Gamescope variables so it starts as the desktop client. Starting a game from the
dashboard or a pin starts Steam on WayVR's displays if it isn't running. Android games run through Lepton: the
shell's `PATH` holds `steamvr`, whose `path` gives Lepton a folder with frame-monado's Android client and a hard
link to Monado's socket (Lepton mounts it as the Android runtime), and `podman`, which uses the user's own session
bus and runtime folder when Steam runs in Plasma. `gamescope-0` links to WayVR's socket (KWin's in Plasma), the
Wayland display Lepton expects.

## Several PCs

A PC is identified by its WiVRn server cookie, which its packaged server and `just run` share
(`FRAME_SHELL_IDENTITY_DIR`). A manual address takes its PC's cookie once connected. Pins store the cookie; screen
windows are keyed `<cookie> <connector>` (known ones in `~/.config/wayvr/frame-shell-screens` on the headset).
PCs that connected once stay listed while offline.

The background PC (the newest started VR app, or one locked from its context menu) presents VR and gets the
microphone. Other connected PCs keep their desktops and speaker audio; while one of their overlays is visible, their
runtime hides their background app and they stream just the overlays with alpha. The headset composes all of them
in one OpenXR frame, background first, so a game from one PC can show with overlays from others. Secondary overlay
streams need the host runtime's background-visibility API (libmonado 1.12). Clicking the active VR app hides it,
clicking it again shows it; an overlay badge marks overlays.

## Source map

| What | Edit here |
| --- | --- |
| WiVRn client, server and shared wire protocol | `wivrn/` |
| Embedded shell, watch, windows, audio, floor UI | `wayvr/wayvr/src/` |
| PCs and their screens | `wayvr/wayvr/src/frame_hosts.rs` |
| Dashboard UI and layouts | `wayvr/dash-frontend/src/`, `wayvr/dash-frontend/assets/gui/` |
| PC host services: monitor capture, WayVR IPC, notifications, metrics, games | `desktop/src/` |
| Shell ↔ host messages | `wayvr/wayvr-ipc/src/frame.rs` |
| C ABIs (headset shell, PC host) | `wayvr/frame-abi/include/frame_abi.h` |
| Build and deploy recipes | `justfile`, `scripts/`, `Containerfile` |
| Headset launcher and service | `headset/` |

These are ordinary tracked source directories. Edit, build, commit and push here; there are no authoritative
ignored clones or patch export step. Client and server share one `wivrn/common/wivrn_packets.h`. Both C ABIs
are defined once in `frame_abi.h`; Rust gets them through bindgen at build time.

`desktop/` builds `libframe_host.a`, which the server links and starts per headset connection. `capture.rs`
streams monitors; `ipc_proxy.rs` tunnels WayVR IPC clients; `notifications.rs`
forwards VRCX and desktop notifications; `metrics.rs` and `games.rs` cover the watch and dashboard.
Host messages and commands travel through WiVRn's authenticated connection as opaque bytes.

`out/` contains ignored build products, caches and private pairing state. The build scripts
never copy source onto the headset. Headset outputs are under `out/headset/stage/frame-shell/`; the host runtime
manifest is generated at `out/host/openxr_wivrn-dev.json`.

See [ARCHITECTURE.md](ARCHITECTURE.md) for process ownership, ABI, wire messages and external interfaces.

## Monado dependency

The headset runtime is independently built/deployed from `frame-monado`'s `main` branch. `just build-headset`
builds only the WiVRn client and WayVR; `just deploy` restarts only the shell and leaves Monado installed.

The PC server needs WiVRn's different Monado revision. That committed source is on `frame-monado`'s `wivrn`
branch, pinned in this repository's `flake.lock`. No runtime integration patches are applied during our builds.
To work on that dependency locally:

```sh
# In the frame-monado repository:
git worktree add ../frame-monado-host wivrn

# In frame-shell:
FRAME_HOST_MONADO=../frame-monado-host just build-host
```

Once the host runtime change is committed and pushed, update the pinned dependency with
`nix flake update host-monado`, rebuild, and commit the lock file.

## Upstream and checks

WiVRn and WayVR are Git subtrees (squashed imports of upstream WiVRn and the WayVR fork), so their source is in a
normal clone.

Validate host changes with `just build-host`, headset changes with `just build-headset`, and protocol changes with
both. Runtime changes belong in frame-monado. Deployment checks dynamic dependencies before replacing binaries.
Headset logs are available with:

```sh
ssh steamos@frame.local 'journalctl --user -u frame-shell.service -u monado.service -n 100 --no-pager'
```

### Streaming over Wi-Fi

The headset streams over home Wi-Fi only; the Frame's USB dongle isn't used (it lost 3–4× more frames than a good
5 GHz channel; the integration is on the `dongle` branch). The WiVRn tab shows the Wi-Fi link's signal and rates.

Error correction (WiVRn tab, `fec_percent` in the headset's `client.json`, default 10) adds erasure-code
parity to every VR and desktop frame; it applies live and costs only its share of the bitrate. Over home Wi-Fi at
100 Mbit/s, 10% repairs about half the lost frames.

Retransmission: when a frame's packets go missing,
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

The WiVRn tab shows the Wi-Fi link (signal, rates) and the VR stream's freezes over 50 ms in the last 5 s.

### Defoveation in the runtime

With frame-monado's `XR_FRAME_foveated_projection`, the client copies the decoded (foveated) frame 1:1 and attaches
the per-axis foveation segments to the projection layer; Monado expands it in its distortion pass. That skips the
client's full-size pass (1.75 → 0.45 ms GPU) and a 3240² write and read per frame, for ~1 ms more in Monado's pass:
~0.5 W less in the spin test. Alpha streams, the stream GUI's dimming and the foveation tint use the old pass;
`runtime_defoveation: false` in `client.json` forces it.

### Eye expressions

The headset client runs Babble Frame Runtime 1.1's two-stage eye kit (vendored in `wivrn/client/frame_eyes/babble/`,
GPL-3.0; the models are downloaded by `just build-headset`) on the raw eye-camera frames from Valve's `dsp_service`,
through ExecuTorch (cross-built into the build container): stage 1 gives lid openness, pupil centre and widen per eye,
stage 2 the pupil diameter in mm from a full-resolution crop around that centre. Lids are calibrated to the wearer
(neutral, and how far the eyes close) after a few seconds and again each time the headset is put on; widen and
pupil dilation are the mean of both eyes, as Babble sends them by default; per-eye gaze comes from Valve's eye
tracker (`/dev/shm/eye-server.mmap`) with the shell's click calibration. The `from_headset::frame_eyes` packet
carries them over the stream socket, and the server sends them to VRChat's native tracking OSC endpoints: per-eye
gaze as `/tracking/eye/LeftRightPitchYaw` (capped at ±45°, smoothed with a One Euro filter between Babble Frame Runtime's light and middle presets), the mean lid closure as `/tracking/eye/EyesClosedAmount`,
and a `/tracking/face/v1` blob with the matching `EyeLook*` keys (45° is 1), `EyeClosed*`, `EyeWide*`, and pupil size
as `EyeDilation*` above the wearer's baseline and `EyeConstrict*` below it.
The WiVRn tab's eye tracking line shows the update rate and whether it is still calibrating.

- Headset `client.json`: `eye_expressions` (default on), `eye_expressions_divisor` (one model update per that many
  camera frames, default 1). The cameras run at frame-monado's `frame.eye_tracking_fps` (default 36).
  At divisor 1, increasing 15 to 72 Hz measured about +0.5 W and +17% of one CPU core for the eye process
  plus shell in a panel-on idle test.
- Server config: `eye-osc` (`host:port`, default `127.0.0.1:9000`; empty or `false` turns it off).

Server config `tracking-sample-ms` (default 6, upstream 3): the prediction step between the headset's tracking
samples of a device. Each sample is a runtime query and a packet; 6 ms halves them (~0.25 W on the headset in the
spin test), 12 ms saved nothing more.

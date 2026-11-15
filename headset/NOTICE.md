# Bundled software

The headset bundle's parts, their licenses and sources. `VERSION` names the frame-shell and frame-monado
revisions it was built from; the frame-shell repository at that revision holds the source of everything marked
"in frame-shell", including its changes.

| Part | License | Source |
|---|---|---|
| Frame shell: WiVRn client and WayVR | GPL-3.0 (WayVR GPL-3.0-only, WiVRn GPL-3.0-or-later) | in frame-shell (`wivrn/`, `wayvr/`), <https://github.com/konsti219/frame-shell> |
| Babble Frame Runtime eye tracking and its eye models | GPL-3.0-or-later | in frame-shell (`wivrn/client/frame_eyes/babble/`), models from <https://github.com/Project-Babble/Babble-Frame-Runtime> |
| xrizer, patched (`lib/xrizer`) | GPL-3.0-or-later | <https://github.com/Supreeeme/xrizer>, revision and patches in frame-shell's `nix/` |
| Frame Monado (`monado/`, `session/`) | BSL-1.0; third-party parts as listed in `LICENSES/frame-monado` | <https://github.com/konsti219/frame-monado> |
| xwayland-satellite | MPL-2.0 | <https://github.com/Supreeeme/xwayland-satellite>, v0.8.3 |
| OpenXR loader, libktx (`lib/`) | Apache-2.0 | <https://github.com/KhronosGroup/OpenXR-SDK>, <https://github.com/KhronosGroup/KTX-Software> |
| libjpeg-turbo (`monado/lib`) | IJG, BSD-3-Clause, Zlib | Steam Runtime 4 SDK |

SteamVR's Frame driver and XRService are not part of the bundle: Monado loads them from the headset's SteamVR.

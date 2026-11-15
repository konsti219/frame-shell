# Frame shell

A standalone shell for the Valve Steam Frame, running on [Frame Monado](https://github.com/konsti219/frame-monado)
instead of SteamVR, built from [WayVR](https://github.com/wlx-team/wayvr) and [WiVRn](https://github.com/WiVRn/WiVRn).

Features:

- dashboard, keyboard and watch
- native Wayland and X11 windows, and a nested Plasma desktop
- Steam and its games: native, Proton/FEX, and Android through Lepton
- PC VR streaming over Wi-Fi, with error correction, retransmission and foveation from the eye gaze
- the PCs' monitors as windows, with laser and keyboard input
- several PCs at once: one presents VR, the others' overlays on top, audio from all of them
- passthrough, floor setup and eye calibration
- eye gaze and expressions to VRChat over OSC, with [Babble Frame Runtime](https://github.com/Project-Babble/Babble-Frame-Runtime)'s eye model
- battery warnings and a performance mode for streaming

Unofficial and experimental.

## Install

Headset: run the following commands (either over SSH or directly in the headset). This will ask for sudo during one step.

```sh
curl -fL https://github.com/konsti219/frame-shell/releases/latest/download/frame-shell.tar.zst | tar --zstd -x
frame-shell/install.sh
```

This installs the shell with [Frame Monado](https://github.com/konsti219/frame-monado) in
`~/.local/share/frame-shell`, and registers Monado as a SteamOS session (the one step that asks for the sudo
password). Running a newer release's installer updates it. Running `~/.local/share/frame-shell/install.sh uninstall`
removes everything again (`--purge` also removes settings and pairings). Building it yourself:
[DEVELOPMENT.md](DEVELOPMENT.md).

PC (NixOS):

```nix
# flake inputs
frame-shell.url = "github:konsti219/frame-shell";

# configuration
imports = [inputs.frame-shell.nixosModules.host];
services.frame-shell = {
  enable = true;
  inputUsers = ["you"];    # get the headset's keyboard and mouse
  # optional: Steam games from the shell, OpenVR games through the flake's patched xrizer
  steamPackage = config.programs.steam.package;
  settings.openvr-compat-path = "${inputs.frame-shell.packages.x86_64-linux.xrizer}/lib/xrizer";
};
```

This runs the WiVRn server with the shell's host services (monitor capture, input, notifications) as a user
service, and opens its port. The headset finds PCs over mDNS, the first connection is paired with a PIN, as in WiVRn.

## Switching

After installation, an application is added that you can "start" from SteamVR's "Launch Program" menu, which
switches the headset to the Monado session. The frame shell's power menu has an option to switch back to SteamVR.

## More

- [DEVELOPMENT.md](DEVELOPMENT.md): building, deploying, the dev server and the source layout
- [ARCHITECTURE.md](ARCHITECTURE.md): processes, ABIs and wire messages

`wivrn/` and `wayvr/` are Git subtrees of upstream WiVRn and a WayVR fork, changed in place. The eye tracking in
`wivrn/client/frame_eyes/babble/` is vendored from Babble Frame Runtime.

## License

GPL-3.0 (WayVR is GPL-3.0-only, WiVRn and Babble Frame Runtime GPL-3.0-or-later); see [LICENSE](LICENSE).

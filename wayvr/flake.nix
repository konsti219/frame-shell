{
  description = "Standalone WayVR flake for local development and packaging";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    crane,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachSystem ["x86_64-linux" "aarch64-linux"] (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
      };
      lib = pkgs.lib;
      # withOpenVR = system != "aarch64-linux";

      uiDevRuntimeLibs = [
        pkgs.libGL
        pkgs.libx11
        pkgs.libxkbcommon
        pkgs.vulkan-loader
        pkgs.wayland
      ];

      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = [
          "clippy"
          "rust-src"
          "rustfmt"
        ];
      };

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
      src = ./.;

      features = lib.concatStringsSep "," (
        [
          "openxr"
          "osc"
          "wayland"
          "x11"
          "feat-monado-metrics"
          "whisper"
        ]
        # ++ lib.optionals withOpenVR ["openvr"]
      );

      commonArgs = {
        inherit src;
        strictDeps = true;
        pname = "wayvr";
        version = "26.2.1";

        cargoExtraArgs = "--package wayvr --no-default-features --features ${features}";

        nativeBuildInputs = [
          pkgs.cmake
          pkgs.pkg-config
          pkgs.rustPlatform.bindgenHook
          # glslc, for whisper-rs's Vulkan GGML backend (GGML_VULKAN=ON)
          pkgs.shaderc
        ];

        buildInputs =
          [
            pkgs.alsa-lib
            pkgs.dav1d
            pkgs.dbus
            # libinput + udev, for the `input` crate used by input capture
            pkgs.libinput
            pkgs.libxkbcommon
            pkgs.libx11
            pkgs.libxext
            pkgs.libxrandr
            pkgs.libxcb
            pkgs.onnxruntime
            pkgs.openssl
            pkgs.openxr-loader
            pkgs.pipewire
            pkgs.shaderc
            pkgs.systemdLibs
            pkgs.vulkan-headers
            pkgs.vulkan-loader
          ]
          # ++ lib.optionals withOpenVR [pkgs.openvr]
          ;

        env.SHADERC_LIB_DIR = "${lib.getLib pkgs.shaderc}/lib";
        env.CMAKE_ARGS = "-DCMAKE_POLICY_VERSION_MINIMUM=3.5";
        # Force ort-sys to use the system ONNX Runtime from nixpkgs
        env.ORT_STRATEGY = "system";
        env.ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
        env.ORT_PREFER_DYNAMIC_LINK = "1";

        # libspa-sys and pipewire-sys write their bindgen output beside their
        # sources, but Crane's vendored dependencies live in the read-only Nix
        # store. Rebuild the vendored source dir with writable copies of those
        # crates (symlinking everything else) and point cargo at it.
        preBuild = ''
          vendorSrc="$(dirname "$(find -L "$cargoVendorDir" -mindepth 2 -maxdepth 2 -type d -name 'libspa-sys-*' | head -n1)")"
          writableVendor="$TMPDIR/writable-vendor"
          mkdir -p "$writableVendor"
          for entry in "$vendorSrc"/*; do
            name="$(basename "$entry")"
            case "$name" in
              libspa-sys-*|pipewire-sys-*)
                cp -aL "$entry" "$writableVendor/$name"
                chmod -R u+w "$writableVendor/$name"
                ;;
              *)
                ln -sn "$(readlink -f "$entry")" "$writableVendor/$name"
                ;;
            esac
          done
          substituteInPlace "$CARGO_HOME/config.toml" \
            --replace-fail "$vendorSrc" "$writableVendor"
        '';

        postPatch = ''
          if [[ -f wlx-common/src/steam.rs ]]; then
            substituteInPlace wlx-common/src/steam.rs \
              --replace-fail 'Command::new("pkill")' 'Command::new("${lib.getExe' pkgs.procps "pkill"}")'
          fi
          if [[ -f wayvr/src/gui/panel/button.rs ]]; then
            substituteInPlace wayvr/src/gui/panel/button.rs \
              --replace-fail 'Command::new("wivrnctl")' 'Command::new("${lib.getExe' pkgs.wivrn "wivrnctl"}")'
          fi
        '';

        # postPatch = ''
        #   substituteAllInPlace dash-frontend/src/util/pactl_wrapper.rs \
        #     --replace-fail '"pactl"' '"${lib.getExe' pkgs.pulseaudio "pactl"}"'

        #   # steam_utils also calls xdg-open as well as steam. Those should probably be pulled from the environment
        #   substituteInPlace dash-frontend/src/util/steam_utils.rs \
        #     --replace-fail '"pkill"' '"${lib.getExe' pkgs.procps "pkill"}"'
        # '';
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      wayvrPkg = craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;

          postInstall = ''
            install -D wayvr/wayvr.desktop -t $out/share/applications
            install -D wayvr/wayvr.svg -t $out/share/icons/hicolor/scalable/apps
          '';

          meta = {
            description = "Your way to enjoy VR on Linux! Access your Wayland/X11 desktop from SteamVR/Monado (OpenVR+OpenXR support)";
            homepage = "https://github.com/wlx-team/wayvr";
            license = with lib.licenses; [
              gpl3Only
              mit
            ];
            platforms = lib.platforms.linux;
            mainProgram = "wayvr";
          };
        }
      );
    in {
      packages = {
        default = wayvrPkg;
        wayvr = wayvrPkg;
      };

      apps.default = {
        type = "app";
        program = "${wayvrPkg}/bin/wayvr";
      };

      devShells.default = pkgs.mkShell {
        inputsFrom = [wayvrPkg];
        packages =
          [
            rustToolchain
          ]
          ++ uiDevRuntimeLibs;

        shellHook = ''
          export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
          export SHADERC_LIB_DIR="${lib.getLib pkgs.shaderc}/lib"
          export CMAKE_ARGS="-DCMAKE_POLICY_VERSION_MINIMUM=3.5"
          export ORT_STRATEGY="system"
          export ORT_LIB_LOCATION="${pkgs.onnxruntime}/lib"
          export ORT_PREFER_DYNAMIC_LINK="1"
          export LD_LIBRARY_PATH="${lib.makeLibraryPath uiDevRuntimeLibs}:''${LD_LIBRARY_PATH:-}"
        '';
      };

      formatter = pkgs.nixpkgs-fmt;
    });
}

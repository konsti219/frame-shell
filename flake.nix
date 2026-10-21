{
  description = "Frame shell host tools and development environment";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    host-monado = {
      url = "git+ssh://github.com_konsti219/konsti219/frame-monado.git?ref=wivrn";
      flake = false;
    };
  };
  outputs = { self, nixpkgs, flake-utils, crane, rust-overlay, host-monado }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
    wayvr = (import ./wayvr/flake.nix).outputs {
      inherit nixpkgs flake-utils crane rust-overlay;
      self = { outPath = ./wayvr; };
    };
    rustToolchain = pkgs.rust-bin.stable.latest.default;
    rustPlatform = pkgs.makeRustPlatform { cargo = rustToolchain; rustc = rustToolchain; };
    gstPlugins = with pkgs.gst_all_1; [ gstreamer.out gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgs.pipewire ];
    hostLibs = with pkgs; [ dbus pipewire wayland libxkbcommon gst_all_1.gstreamer gst_all_1.gst-plugins-base ];
    # The PC host services, a static library linked into the server
    frame-host = rustPlatform.buildRustPackage {
      pname = "frame-host";
      version = "0.1.0";
      src = ./desktop;
      cargoLock.lockFile = ./desktop/Cargo.lock;
      postPatch = ''
        substituteInPlace Cargo.toml --replace-fail '../wayvr/' '${./wayvr}/'
      '';
      nativeBuildInputs = [ pkgs.pkg-config pkgs.rustPlatform.bindgenHook ];
      buildInputs = hostLibs;
      doCheck = false;
      installPhase = ''
        runHook preInstall
        install -Dm644 "$(find target -path '*/release/libframe_host.a' | head -1)" $out/lib/libframe_host.a
        runHook postInstall
      '';
    };
    craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
    # PC-side WayVR IPC clients, built from our tree so they speak the headset's protocol
    ipcClient = pname: craneLib.buildPackage {
      inherit pname;
      version = "0.1.0";
      src = ./wayvr;
      strictDeps = true;
      cargoExtraArgs = "--package ${pname}";
      doCheck = false;
    };
    # Firefox native-messaging host for the watch's media controls; register via programs.firefox.nativeMessagingHosts
    media-bridge = (ipcClient "wayvr-media-bridge").overrideAttrs {
      postInstall = ''
        mkdir -p $out/lib/mozilla/native-messaging-hosts
        substitute ${./wayvr/extras/firefox-ytmusic/native-host/dev.wayvr.ytmusic.json} \
          $out/lib/mozilla/native-messaging-hosts/dev.wayvr.ytmusic.json \
          --replace-fail @BRIDGE_PATH@ $out/bin/wayvr-media-bridge
      '';
      meta.mainProgram = "wayvr-media-bridge";
    };
    # The browser add-on as an unsigned .xpi, named after its gecko id
    ytmusic-extension = pkgs.runCommand "wayvr-ytmusic-extension" { nativeBuildInputs = [ pkgs.zip ]; } ''
      mkdir -p $out
      cd ${./wayvr/extras/firefox-ytmusic/extension}
      zip -r -X "$out/wayvr-ytmusic@konsti.xpi" .
    '';
    wayvrctl = ipcClient "wayvrctl";
    # OpenVR games on WiVRn, with the Frame's controllers
    xrizer = pkgs.xrizer.overrideAttrs (finalAttrs: old: {
      version = "0.6-unstable-2026-10-14";
      src = pkgs.fetchFromGitHub {
        owner = "Supreeeme";
        repo = "xrizer";
        rev = "63fc7eb3f80bba2277b48c023d2af569cc6e6453";
        hash = "sha256-/UiAJhKrMN2gYD3aVeORtI196xzjs61cK/FLyHPizZw=";
      };
      cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
        inherit (finalAttrs) src;
        hash = "sha256-HVeKLohd73oBoiQHZE5jGwDJeLvR4J8I5zRnYvZlHoA=";
      };
      patches = (old.patches or [ ]) ++ [
        # Native Steam Frame controller profile, plus custom bindings for undeclared controllers
        ./nix/xrizer-frame-profile.patch
        # Supreeeme/xrizer#338: Partial (not Full) skeletal level for controller-sourced hand joints
        ./nix/xrizer-hand-tracking-data-source.patch
        # SteamVR's fixed wrist for controller skeletons (the Frame's wrist joint is pitched ~27°)
        ./nix/xrizer-controller-wrist.patch
        # Finger curl from summed joint bends (pinky under-curled before)
        ./nix/xrizer-finger-curl.patch
        # Some apps ignore right hand inputs when bound on both hands, merge both into the left query
        ./nix/xrizer-both-hand-binds.patch
        # main dlopens a bare libopenxr_loader.so, which isn't findable (esp. in Proton); link it instead
        ./nix/xrizer-linked-openxr.patch
      ];
      postPatch = ''
        substituteInPlace src/graphics_backends/gl.rs \
          --replace-fail 'libGLX.so.0' '${pkgs.lib.getLib pkgs.libglvnd}/lib/libGLX.so.0'
      '';
      # main derives its version from git, which the source tarball lacks
      env = (old.env or { }) // { XRIZER_VERSION = "${finalAttrs.version}-frame-shell"; };
    });
    server = pkgs.wivrn.overrideAttrs (old: {
      pname = "wivrn-frame-shell";
      version = "unstable-2026-10-02";
      src = ./wivrn;
      patches = [];
      postPatch = "";
      postUnpack = "";
      monado = host-monado;
      cmakeFlags = builtins.filter (flag: !(builtins.any (prefix: pkgs.lib.hasPrefix prefix flag)
          [ "-DFETCHCONTENT_SOURCE_DIR_MONADO" "-DGIT_TAG:" "-DGIT_DESC:" "-DGIT_COMMIT:" ])) old.cmakeFlags
        ++ [
          (pkgs.lib.cmakeFeature "GIT_DESC" "7e0bf5e")
          (pkgs.lib.cmakeFeature "GIT_COMMIT" "7e0bf5efc1f4298b433eff1c6f3d81a1bc401b44")
          (pkgs.lib.cmakeFeature "FETCHCONTENT_SOURCE_DIR_MONADO" "${host-monado}")
          (pkgs.lib.cmakeFeature "FRAME_HOST_LIBRARY" "${frame-host}/lib/libframe_host.a")
          (pkgs.lib.cmakeFeature "FRAME_ABI_INCLUDE_DIR" "${./wayvr/frame-abi/include}")
        ];
      # CompileGLSL.cmake embeds shaders via hexdump; the dashboard needs kirigami-addons' formcard
      nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.makeWrapper pkgs.unixtools.hexdump ];
      buildInputs = old.buildInputs ++ [ pkgs.kdePackages.kirigami-addons ] ++ hostLibs;
      postFixup = (old.postFixup or "") + ''
        wrapProgram $out/bin/wivrn-server --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins}
      '';
    });
  in {
    nixosModules.host = import ./nix/host-module.nix self;
    packages.${system} = {
      default = server;
      inherit server frame-host xrizer media-bridge ytmusic-extension wayvrctl;
    };
    devShells.${system}.default = pkgs.mkShell {
      inputsFrom = [ server wayvr.devShells.${system}.default ];
      GST_PLUGIN_SYSTEM_PATH_1_0 = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins;
      LIBVA_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
      packages = with pkgs; [ cmake ninja gdb pkg-config python3 shellcheck gst_all_1.gstreamer gst_all_1.gst-plugins-base ];
    };
  };
}

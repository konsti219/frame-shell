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
  outputs = {
    self,
    nixpkgs,
    flake-utils,
    crane,
    rust-overlay,
    host-monado,
  }: let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
    wayvr = (import ./wayvr/flake.nix).outputs {
      inherit nixpkgs flake-utils crane rust-overlay;
      self = {outPath = ./wayvr;};
    };
    gstPlugins = with pkgs.gst_all_1; [gstreamer.out gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgs.pipewire];
    hostLibs = with pkgs; [dbus pipewire wayland libxkbcommon gst_all_1.gstreamer gst_all_1.gst-plugins-base];
    # The PC host services, a static library linked into the server
    frame-host = pkgs.rustPlatform.buildRustPackage {
      pname = "frame-host";
      version = "0.1.0";
      src = ./desktop;
      cargoLock.lockFile = ./desktop/Cargo.lock;
      postPatch = ''
        substituteInPlace Cargo.toml --replace-fail '../wayvr/' '${./wayvr}/'
      '';
      nativeBuildInputs = [pkgs.pkg-config pkgs.rustPlatform.bindgenHook];
      buildInputs = hostLibs;
      doCheck = false;
      installPhase = ''
        runHook preInstall
        install -Dm644 "$(find target -path '*/release/libframe_host.a' | head -1)" $out/lib/libframe_host.a
        runHook postInstall
      '';
    };
    # PC-side WayVR IPC client, built from our tree so it speaks the headset's protocol
    wayvrctl = pkgs.rustPlatform.buildRustPackage {
      pname = "wayvrctl";
      version = "0.1.0";
      src = ./wayvr;
      cargoHash = "sha256-z2NfEYrpzXP5IA0lQ4vFBEbi0dBH/pmxiP/1lWvuo5A=";
      cargoBuildFlags = ["--package" "wayvrctl"];
      doCheck = false;
    };
    xrizerSource = builtins.fromJSON (builtins.readFile ./nix/xrizer.json);
    # OpenVR games on WiVRn, with the Frame's controllers
    xrizer = pkgs.xrizer.overrideAttrs (finalAttrs: old: {
      version = xrizerSource.version;
      src = pkgs.fetchFromGitHub {
        inherit (xrizerSource) owner repo rev hash;
      };
      cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
        inherit (finalAttrs) src patches;
        hash = xrizerSource.cargoHash;
      };
      patches = (old.patches or []) ++ map (name: ./nix + "/${name}") xrizerSource.patches;
      postPatch = ''
        substituteInPlace src/graphics_backends/gl.rs \
          --replace-fail 'libGLX.so.0' '${pkgs.lib.getLib pkgs.libglvnd}/lib/libGLX.so.0'
      '';
      # main derives its version from git, which the source tarball lacks
      env = (old.env or {}) // {XRIZER_VERSION = "${finalAttrs.version}-frame-shell";};
    });
    server = pkgs.wivrn.overrideAttrs (old: {
      pname = "frame-shell-server";
      # Our own revision: the version shown in the headset's dashboard is the installed frame-shell commit
      version = "unstable-${builtins.substring 0 8 self.lastModifiedDate}";
      src = ./wivrn;
      patches = [];
      postPatch = "";
      postUnpack = "";
      monado = host-monado;
      cmakeFlags =
        builtins.filter (flag:
          !(builtins.any (prefix: pkgs.lib.hasPrefix prefix flag)
            ["-DFETCHCONTENT_SOURCE_DIR_MONADO" "-DGIT_TAG:" "-DGIT_DESC:" "-DGIT_COMMIT:"]))
        old.cmakeFlags
        ++ [
          (pkgs.lib.cmakeFeature "GIT_DESC" (self.shortRev or self.dirtyShortRev))
          (pkgs.lib.cmakeFeature "GIT_COMMIT" (self.rev or self.dirtyRev))
          (pkgs.lib.cmakeFeature "FETCHCONTENT_SOURCE_DIR_MONADO" "${host-monado}")
          (pkgs.lib.cmakeFeature "FRAME_HOST_LIBRARY" "${frame-host}/lib/libframe_host.a")
          (pkgs.lib.cmakeFeature "FRAME_ABI_INCLUDE_DIR" "${./wayvr/frame-abi/include}")
        ];
      # CompileGLSL.cmake embeds shaders via hexdump; the dashboard needs kirigami-addons' formcard
      nativeBuildInputs = old.nativeBuildInputs ++ [pkgs.makeWrapper pkgs.unixtools.hexdump];
      buildInputs = old.buildInputs ++ [pkgs.kdePackages.kirigami-addons] ++ hostLibs;
      postFixup =
        (old.postFixup or "")
        + ''
          wrapProgram $out/bin/wivrn-server --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins}
        '';
    });
  in {
    nixosModules.host = import ./nix/host-module.nix self;
    packages.${system} = {
      default = server;
      inherit server frame-host xrizer wayvrctl;
    };
    devShells.${system}.default = pkgs.mkShell {
      inputsFrom = [server wayvr.devShells.${system}.default];
      GST_PLUGIN_SYSTEM_PATH_1_0 = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins;
      LIBVA_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
      packages = with pkgs; [cmake ninja gdb pkg-config python3 shellcheck gst_all_1.gstreamer gst_all_1.gst-plugins-base];
    };
  };
}

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
    gstPlugins = with pkgs.gst_all_1; [ gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgs.pipewire ];
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
    server = wayvr.packages.${system}.wivrn.overrideAttrs (old: {
      pname = "wivrn-frame-shell";
      src = ./wivrn;
      patches = [];
      postPatch = "";
      postUnpack = "";
      monado = host-monado;
      cmakeFlags = builtins.filter (flag: !(pkgs.lib.hasPrefix "-DFETCHCONTENT_SOURCE_DIR_MONADO" flag)) old.cmakeFlags
        ++ [
          (pkgs.lib.cmakeFeature "FETCHCONTENT_SOURCE_DIR_MONADO" "${host-monado}")
          (pkgs.lib.cmakeFeature "FRAME_HOST_LIBRARY" "${frame-host}/lib/libframe_host.a")
          (pkgs.lib.cmakeFeature "FRAME_ABI_INCLUDE_DIR" "${./wayvr/frame-abi/include}")
        ];
      nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.makeWrapper ];
      buildInputs = old.buildInputs ++ hostLibs;
      postFixup = (old.postFixup or "") + ''
        wrapProgram $out/bin/wivrn-server --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins}
      '';
    });
  in {
    packages.${system} = { default = server; inherit server frame-host; };
    devShells.${system}.default = pkgs.mkShell {
      inputsFrom = [ server wayvr.devShells.${system}.default ];
      GST_PLUGIN_SYSTEM_PATH_1_0 = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins;
      LIBVA_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
      packages = with pkgs; [ cmake ninja gdb pkg-config python3 shellcheck gst_all_1.gstreamer gst_all_1.gst-plugins-base ];
    };
  };
}

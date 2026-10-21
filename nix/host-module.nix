# The frame shell's PC host on NixOS: the WiVRn server with the host services (monitor streaming, WayVR IPC
# tunnel, notifications) as a user service, discoverable and reachable by the headset.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.frame-shell;
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
  json = pkgs.formats.json { };
  configFile = json.generate "frame-shell-server.json" cfg.settings;
in
{
  options.services.frame-shell = {
    enable = lib.mkEnableOption "the frame shell's PC host";
    package = lib.mkOption {
      type = lib.types.package;
      default = packages.server;
      description = "The WiVRn server with the frame shell's host services.";
    };
    autoStart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Start the server with each user session.";
    };
    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Open WiVRn's port (9757, TCP and UDP).";
    };
    inputUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Users whose sessions get the headset's keyboard and mouse (uinput access).";
    };
    steamPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Steam in the server's PATH, to start Steam games from the shell.";
    };
    settings = lib.mkOption {
      type = json.type;
      default = { };
      description = "WiVRn server configuration (its config.json).";
    };
  };

  config = lib.mkIf cfg.enable {
    services.frame-shell.settings = {
      # WayVR runs on the headset
      application = lib.mkDefault [ ];
      hid-forwarding = lib.mkDefault true;
      # Encoder rate-control window: smaller bursts on Wi-Fi
      encoder = lib.mkDefault [
        { options.buffer_ms = "50"; }
        { options.buffer_ms = "50"; }
      ];
    };

    systemd.user.services.frame-shell = {
      description = "Frame shell PC host (WiVRn server)";
      unitConfig.ConditionUser = "!@system";
      path = lib.optional (cfg.steamPackage != null) cfg.steamPackage;
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "wivrn-server"} -f ${configFile}";
        Restart = "on-failure";
      };
      wantedBy = lib.mkIf cfg.autoStart [ "default.target" ];
      restartTriggers = [ cfg.package configFile ];
    };

    # The headset finds PCs through mDNS
    services.avahi = {
      enable = true;
      publish = {
        enable = true;
        userServices = true;
      };
    };

    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = [ 9757 ];
      allowedUDPPorts = [ 9757 ];
    };

    hardware.uinput.enable = true;
    users.users = lib.genAttrs cfg.inputUsers (_: { extraGroups = [ "uinput" ]; });

    environment = {
      # Scripts driving the shell's panels (panel-modify, listen-events) through the IPC tunnel
      systemPackages = [ cfg.package packages.wayvrctl ];
      # The server's OpenXR runtime manifest, which it makes the active runtime while a headset is connected
      pathsToLink = [ "/share/openxr" ];
    };
  };
}

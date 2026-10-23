{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.floppa-vpn;
in
{
  options.services.floppa-vpn = {
    enable = lib.mkEnableOption "Floppa VPN socket-activated tunnel service and standalone clients";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.lib.mkCli { inherit pkgs; };
    };
    guiPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "GUI built with lib.mkClient and a deployment API endpoint.";
    };
    keepBuildDependencies = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Keep Crane dependency artifacts in the system closure to speed up local rebuilds after garbage collection.";
    };
    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Users allowed to control the tunnel or run standalone with CAP_NET_ADMIN.";
    };
    autostart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Run resume --if-enabled on boot; the saved preference still decides whether to connect.";
    };
  };
  config = lib.mkIf cfg.enable {
    system.extraDependencies = lib.optionals cfg.keepBuildDependencies (
      [ cfg.package.cargoArtifacts ]
      ++ lib.optional (cfg.package.resources ? cargoArtifacts) cfg.package.resources.cargoArtifacts
      ++ lib.optional (cfg.guiPackage != null) cfg.guiPackage.cargoArtifacts
    );
    services.resolved.enable = lib.mkDefault true;
    networking.networkmanager.dns = lib.mkIf config.networking.networkmanager.enable (
      lib.mkDefault "systemd-resolved"
    );
    users.groups.floppa = { };
    users.users = lib.genAttrs cfg.users (_: {
      extraGroups = [ "floppa" ];
    });
    environment.systemPackages = [
      cfg.package
      cfg.package.resources
    ]
    ++ lib.optional (cfg.guiPackage != null) cfg.guiPackage;
    security.polkit.enable = true;
    security.polkit.enablePkexecWrapper = true;
    security.wrappers = {
      floppa = {
        source = "${cfg.package}/bin/floppa";
        owner = "root";
        group = "floppa";
        permissions = "u+rx,g+rx,o-rwx";
        capabilities = "cap_net_admin+eip";
      };
    }
    // lib.optionalAttrs (cfg.guiPackage != null) {
      floppa-client = {
        source = "${cfg.guiPackage}/bin/floppa-client";
        owner = "root";
        group = "floppa";
        permissions = "u+rx,g+rx,o-rwx";
        capabilities = "cap_net_admin+eip";
      };
    };
    systemd.tmpfiles.rules = lib.splitString "\n" (
      builtins.readFile ../floppa/resources/tmpfiles.d/floppa-vpn.conf
    );
    systemd.sockets.floppa-vpn = {
      description = "Floppa VPN control socket";
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "/run/floppa-vpn/vpn.sock";
        SocketUser = "root";
        SocketGroup = "floppa";
        SocketMode = "0660";
      };
    };
    systemd.services.floppa-vpn = {
      description = "Floppa VPN tunnel service";
      requires = [ "floppa-vpn.socket" ];
      after = [
        "floppa-vpn.socket"
        "network.target"
      ];
      serviceConfig = {
        Type = "exec";
        ExecStart = "${cfg.package}/bin/floppa service";
        StateDirectory = "floppa-vpn";
        StateDirectoryMode = "0700";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };
    systemd.services.floppa-vpn-autostart = lib.mkIf cfg.autostart {
      description = "Resume Floppa VPN on boot if enabled in settings";
      wantedBy = [ "multi-user.target" ];
      requires = [ "floppa-vpn.socket" ];
      wants = [ "network-online.target" ];
      after = [
        "floppa-vpn.socket"
        "network-online.target"
      ];
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/floppa resume --if-enabled";
        RemainAfterExit = true;
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}

# NixOS Module for Halley Wayland Compositor
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.halley;
in {
  options.programs.halley = {
    enable = lib.mkEnableOption "Halley, a spatial Wayland compositor";

    package = lib.mkPackageOption pkgs "halley" {
      default = ["halley"];
    };

    extraPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [];
      example = lib.literalExpression "[ pkgs.foot pkgs.dmenu-wayland ]";
      description = "Extra packages to make available in the system environment alongside Halley.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package] ++ cfg.extraPackages;

    # Expose the Wayland session desktop file to display managers (SDDM, GDM, greetd, etc.)
    services.displayManager.sessionPackages = [cfg.package];

    # Register systemd user units provided by Halley (halley.service, etc.)
    systemd.packages = [cfg.package];

    # Register D-Bus portal backend service file
    services.dbus.packages = [cfg.package];

    # XDG Desktop Portal integration for screenshot and screencast
    xdg.portal = {
      enable = lib.mkDefault true;
      extraPortals = [cfg.package];
      configPackages = [cfg.package];
    };

    # Polkit and hardware graphics are required for seat/DRM management
    security.polkit.enable = lib.mkDefault true;
    hardware.graphics.enable = lib.mkDefault true;
  };
}

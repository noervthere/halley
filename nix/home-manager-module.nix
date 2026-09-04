# Home Manager Module for Halley Wayland Compositor
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

    extraConfig = lib.mkOption {
      type = lib.types.nullOr lib.types.lines;
      default = null;
      example = ''
        # Custom halley.rune settings
        input {
          repeat_delay = 250;
          repeat_rate = 35;
        }
      '';
      description = ''
        Configuration to write to `$XDG_CONFIG_HOME/halley/halley.rune`.
        If null, Halley defaults to its built-in configuration.
      '';
    };

    systemd = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = pkgs.stdenv.isLinux;
        description = "Whether to register Halley systemd user units.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [cfg.package];

    # Write custom halley.rune configuration if provided
    xdg.configFile."halley/halley.rune" = lib.mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };

    # Expose the Wayland session desktop entry
    xdg.dataFile."wayland-sessions/halley.desktop".source =
      "${cfg.package}/share/wayland-sessions/halley.desktop";

    # Native portal backend metadata so xdg-desktop-portal picks up Halley ScreenCast & Screenshot
    xdg.dataFile."xdg-desktop-portal/portals/halley.portal".source =
      "${cfg.package}/share/xdg-desktop-portal/portals/halley.portal";
    xdg.dataFile."xdg-desktop-portal/halley-portals.conf".source =
      "${cfg.package}/share/xdg-desktop-portal/halley-portals.conf";

    # Systemd user services and targets
    xdg.configFile."systemd/user/halley.service" = lib.mkIf cfg.systemd.enable {
      source = "${cfg.package}/lib/systemd/user/halley.service";
    };
    xdg.configFile."systemd/user/halley-shutdown.target" = lib.mkIf cfg.systemd.enable {
      source = "${cfg.package}/lib/systemd/user/halley-shutdown.target";
    };
    xdg.configFile."systemd/user/xdg-desktop-portal-halley.service" = lib.mkIf cfg.systemd.enable {
      source = "${cfg.package}/lib/systemd/user/xdg-desktop-portal-halley.service";
    };
  };
}

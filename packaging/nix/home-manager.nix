{ config, lib, pkgs, ... }:

let
  cfg = config.services.pods;
  unit = "${cfg.package}/share/systemd/user/podsd.service";
in {
  options.services.pods = {
    enable = lib.mkEnableOption "the Pods AirPods user service";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "Pods package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    xdg.configFile."systemd/user/podsd.service".source = unit;
    xdg.configFile."systemd/user/graphical-session.target.wants/podsd.service".source = unit;
  };
}

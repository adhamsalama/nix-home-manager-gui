# Sample home-manager configuration used in mock mode.
# Edits made in the GUI are written to this file for real.
{ config, pkgs, ... }:

{
  home.username = "demo";
  home.homeDirectory = "/Users/demo";
  home.stateVersion = "24.05"; # Read the release notes before changing.

  programs.home-manager.enable = true;

  # Version control
  programs.git = {
    enable = true;
    userName = "Demo User";
    userEmail = "demo@example.com";
    aliases = {
      co = "checkout";
      st = "status";
    };
  };

  programs.zsh = {
    enable = true;
    autosuggestion.enable = true;
  };

  home.packages = with pkgs; [
    ripgrep
    fd
    jq # JSON on the command line
  ];
}

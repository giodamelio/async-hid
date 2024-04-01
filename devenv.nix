{ pkgs, ... }: {
  languages.nix.enable = true;
  languages.rust.enable = true;

  pre-commit.hooks.cargo-check.enable = true;
  pre-commit.hooks.clippy.enable = true;
  pre-commit.hooks.rustfmt.enable = true;

  packages = with pkgs; [
    libusb
  ];

  difftastic.enable = true;

  enterTest = ''
    cargo test
  '';
}

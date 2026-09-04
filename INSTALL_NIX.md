# Installing Halley with Nix Flakes

This repository provides a Nix Flake for building, running, and configuring the **Halley** spatial Wayland compositor (v0.7.0), including its companion utilities (`halleyctl`, `halley-lift`, and `xdg-desktop-portal-halley`).

---

## Quick Start

### 1. Try Without Installing

Run Halley nested inside your current desktop session (requires Wayland/X11 with DRM/EGL):

```bash
nix run github:noervthere/halley -- --winit
```

Or control an active Halley session with `halleyctl`:

```bash
nix run github:noervthere/halley#halleyctl -- --help
```

Launch the search/app launcher:

```bash
nix run github:noervthere/halley#halley-lift
```

### 2. Build the Package Locally

```bash
git clone https://github.com/noervthere/halley.git
cd halley
nix build
```

The resulting binaries and session files will be linked in `./result`:
- `./result/bin/halley`
- `./result/bin/halley-session`
- `./result/bin/halleyctl`
- `./result/bin/halley-lift`
- `./result/bin/xdg-desktop-portal-halley`
- `./result/share/wayland-sessions/halley.desktop`

### 3. Install to User Profile (`nix profile`)

```bash
nix profile install github:noervthere/halley
```

---

## NixOS Installation (Flake Module)

The recommended way to install and manage Halley on NixOS is via the included NixOS module.

### 1. Add Halley to `flake.nix` Inputs

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    halley = {
      url = "github:noervthere/halley";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, halley, ... }: {
    nixosConfigurations.myhostname = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        halley.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

### 2. Enable in `configuration.nix`

```nix
{ pkgs, ... }:

{
  programs.halley = {
    enable = true;
    # Optional: add extra packages for your session (e.g. terminal, bar, app launcher)
    extraPackages = with pkgs; [
      foot
      alacritty
    ];
  };

  # Enable a display manager (e.g., greetd with tuigreet or SDDM)
  services.greetd = {
    enable = true;
    settings = {
      default_session = {
        command = "${pkgs.greetd.tuigreet}/bin/tuigreet --time --remember --cmd halley-session";
        user = "greeter";
      };
    };
  };

  # PipeWire is required for ScreenCast / audio
  services.pipewire = {
    enable = true;
    alsa.enable = true;
    pulse.enable = true;
  };
}
```

The NixOS module automatically:
- Registers `halley.desktop` in `services.displayManager.sessionPackages`
- Configures `xdg.portal` to use `xdg-desktop-portal-halley` for ScreenCast and Screenshot
- Registers Halley's systemd user units (`halley.service`, `halley-shutdown.target`)
- Enables `security.polkit` and `hardware.graphics`
- Places `halley`, `halleyctl`, and `halley-lift` on the system `PATH`

---

## Home Manager Installation

Halley provides a Home Manager module compatible with both **NixOS** and **standalone Home Manager on non-NixOS Linux** (Arch Linux, Void, Fedora, Ubuntu, Debian, etc.).

### 1. Add Input to Home Manager Flake

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    halley = {
      url = "github:noervthere/halley";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, home-manager, halley, ... }: {
    homeConfigurations."yourusername" = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        halley.homeManagerModules.default
        ./home.nix
      ];
    };
  };
}
```

### 2. Configure in `home.nix`

```nix
{ pkgs, ... }:

{
  programs.halley = {
    enable = true;

    # Optional: Manage halley.rune directly through Home Manager
    # If omitted, Halley loads ~/.config/halley/halley.rune or default config
    extraConfig = ''
      # Custom Halley Configuration (Rune)
      input {
        repeat_delay = 250;
        repeat_rate = 35;
      }
    '';
  };
}
```

On non-NixOS distros, if your display manager reads `/usr/share/wayland-sessions`, you can create a symlink to Halley's session desktop file:

```bash
sudo ln -sf ~/.nix-profile/share/wayland-sessions/halley.desktop /usr/share/wayland-sessions/halley.desktop
```

---

## Using as an Overlay

If you manage your packages using Nixpkgs overlays:

```nix
{
  nixpkgs.overlays = [
    halley.overlays.default
  ];

  environment.systemPackages = [
    pkgs.halley
  ];
}
```

---

## Development Shell

To hack on Halley locally with all dependencies (Rust, pkg-config, Wayland, DRM, GBM, EGL, PipeWire, libdisplay-info, libseat, bindgen):

```bash
git clone https://github.com/noervthere/halley.git
cd halley
nix develop
```

Once inside the shell, you can use regular Cargo commands:

```bash
cargo build --release
cargo test
cargo run -- --winit
```

All `PKG_CONFIG_PATH`, `LD_LIBRARY_PATH`, and `LIBCLANG_PATH` environment variables are automatically configured in the shell.

---

## ScreenCast & Screenshot Support

Halley includes a native portal backend (`xdg-desktop-portal-halley`) implementing the `org.freedesktop.impl.portal.ScreenCast` and `org.freedesktop.impl.portal.Screenshot` interfaces via zero-copy DMA-BUF PipeWire streams.

Ensure PipeWire and `xdg-desktop-portal` are running in your session:
```bash
systemctl --user status pipewire
systemctl --user status xdg-desktop-portal
```
Halley installs `halley-portals.conf` and `halley.portal` to make xdg-desktop-portal automatically select Halley when the session desktop is `Halley`.

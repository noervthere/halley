# Halley — Rust derivation for the Halley spatial Wayland compositor.
{
lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  dbus,
  wayland,
  libxkbcommon,
  libinput,
  seatd,
  udev,
  libgbm,
  libdrm,
  libglvnd,
  pixman,
  pipewire,
  libdisplay-info_0_3,
  xwayland,
}: let
  smithayHash = "sha256-TV/GTfSvgfVwIFUGoASU7xm38opIBLjLMf1HeNTW07U=";
in
  rustPlatform.buildRustPackage {
    pname = "halley";
    version = "0.7.0";

    src = lib.cleanSourceWith {
      src = ../.;
      filter = path: type:
        let
          base = baseNameOf path;
        in
          !(
            base == "target"
            || base == ".git"
            || lib.hasSuffix ".md" base
            || lib.hasSuffix ".sh" base
          );
    };

    # Smithay is the only git dependency; the rest come from crates.io via Cargo.lock.
    cargoLock = {
      lockFile = ../Cargo.lock;
      outputHashes = {
        "smithay-0.7.0" = smithayHash;
        "smithay-drm-extras-0.1.0" = smithayHash;
      };
    };

    nativeBuildInputs = [
      pkg-config
      makeWrapper
      # input-sys, libseat, and smithay generate bindings via bindgen -> libclang.
      rustPlatform.bindgenHook
    ];

    buildInputs = [
      wayland # smithay's use_system_lib / wayland frontend
      libxkbcommon
      libinput # backend_libinput
      seatd # backend_session_libseat (provides libseat)
      udev # backend_udev
      libgbm # backend_gbm
      libdrm # backend_drm
      libglvnd # backend_egl / renderer_gl
      pixman
      dbus # IPC / portal
      pipewire # libspa-sys / portal screencast
      libdisplay-info_0_3
    ];

    # Tests require a live Wayland display + system fonts -> cannot run in sandbox.
    doCheck = false;

    # EGL / wayland-client link args:
    # smithay loads libEGL (via libglvnd) and libwayland-client through dlopen.
    # Forcing them into DT_NEEDED (resolved via the binary's RPATH, which Nix
    # fills from buildInputs) means Halley finds EGL/wayland WITHOUT an
    # LD_LIBRARY_PATH. We avoid LD_LIBRARY_PATH on the `halley` binary because
    # it is the compositor: everything it spawns (terminal, browsers, games)
    # inherits its environment, and a stray LD_LIBRARY_PATH would mix Nix libs
    # with system drivers and break GLX/Vulkan in games and apps.
    RUSTFLAGS = lib.concatStringsSep " " (
      map (arg: "-C link-arg=" + arg) [
        "-Wl,--push-state,--no-as-needed"
        "-lEGL"
        "-lwayland-client"
        "-Wl,--pop-state"
      ]
    );

    # Wrap halley with xwayland in PATH for native X11 application support.
    # Companion binaries (halleyctl, halley-lift, xdg-desktop-portal-halley) are safe
    # to wrap with LD_LIBRARY_PATH since they do not launch session child processes.
    postFixup = ''
      wrapProgram $out/bin/halley \
        --prefix PATH : ${lib.makeBinPath [xwayland]}

      for bin in halleyctl halley-lift xdg-desktop-portal-halley; do
        if [ -f "$out/bin/$bin" ]; then
          wrapProgram "$out/bin/$bin" \
            --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [
              libglvnd
              wayland
              libxkbcommon
              libgbm
              libdrm
              pipewire
            ]}
        fi
      done
    '';

    # Install the Wayland session script, desktop entry, portal metadata,
    # systemd user units, D-Bus service, and default rune config template.
    postInstall = ''
      # Session script
      install -Dm755 packaging/wayland-sessions/halley-session $out/bin/halley-session
      substituteInPlace $out/bin/halley-session \
        --replace-fail '/usr/bin/halley' "$out/bin/halley"

      # Wayland session desktop entry
      install -Dm644 packaging/wayland-sessions/halley.desktop \
        $out/share/wayland-sessions/halley.desktop
      substituteInPlace $out/share/wayland-sessions/halley.desktop \
        --replace-fail 'Exec=halley-session' "Exec=$out/bin/halley-session" \
        --replace-fail 'TryExec=halley-session' "TryExec=$out/bin/halley-session"

      # Portal configuration & metadata
      install -Dm644 packaging/xdg-desktop-portal/portals/halley.portal \
        $out/share/xdg-desktop-portal/portals/halley.portal
      install -Dm644 packaging/xdg-desktop-portal/halley-portals.conf \
        $out/share/xdg-desktop-portal/halley-portals.conf

      # Systemd user services & targets
      install -Dm644 packaging/systemd-user/halley.service \
        $out/lib/systemd/user/halley.service
      install -Dm644 packaging/systemd-user/halley-shutdown.target \
        $out/lib/systemd/user/halley-shutdown.target
      install -Dm644 packaging/systemd-user/xdg-desktop-portal-halley.service \
        $out/lib/systemd/user/xdg-desktop-portal-halley.service

      substituteInPlace $out/lib/systemd/user/halley.service \
        --replace-fail '/usr/bin/halley' "$out/bin/halley"
      substituteInPlace $out/lib/systemd/user/xdg-desktop-portal-halley.service \
        --replace-fail '/usr/bin/xdg-desktop-portal-halley' "$out/bin/xdg-desktop-portal-halley"

      # D-Bus service
      install -Dm644 packaging/dbus-1/services/org.freedesktop.impl.portal.desktop.halley.service \
        $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.halley.service
      substituteInPlace $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.halley.service \
        --replace-fail '/usr/bin/xdg-desktop-portal-halley' "$out/bin/xdg-desktop-portal-halley"

      # Reference default configuration
      install -Dm644 halley-config/halley.default.rune \
        $out/share/halley/halley.default.rune
    '';

    # Required for NixOS / HM displayManager.sessionPackages detection
    passthru.providedSessions = ["halley"];

    meta = {
      description = "Spatial Wayland compositor built around infinite workspace navigation";
      homepage = "https://github.com/noervthere/halley";
      license = lib.licenses.gpl3Only;
      mainProgram = "halley";
      platforms = lib.platforms.linux;
    };
  }

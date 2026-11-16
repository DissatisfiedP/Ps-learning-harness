# Built from source: only the desktop binary, without tests.
#
# `cargo build` of the whole workspace would also build the app's library
# target (staticlib + cdylib for Android and iOS, two more fat-LTO links), and
# buildRustPackage's default checkPhase would compile every test binary in
# release mode. Building the one binary peaks at about 1.7 GB of build
# directory and 2.5 GB of RAM; the build directory is deleted afterwards.
{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook3,
  copyDesktopItems,
  makeDesktopItem,
  openssl,
  zlib,
  dbus,
  gtk3,
  webkitgtk_4_1,
  libsoup_3,
  librsvg,
  glib-networking,
  xdg-utils,
}:
let
  desktop = import ./desktop-item.nix { inherit makeDesktopItem; };
in
rustPlatform.buildRustPackage {
  pname = "ps-learning-harness";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # What the build reads: the workspace, its crates (prompts and the plugin
  # API are inside crates/harness) and the UI the binary embeds.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../ui
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "ps-learning-harness"
    "--bin"
    "ps-learning-harness"
  ];
  # The suite runs in CI; here it would compile ~17 release test binaries.
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook3
    copyDesktopItems
  ];
  buildInputs = [
    openssl
    zlib
    # Tauri's windowing crate (tao) links libdbus-1.
    dbus
    gtk3
    webkitgtk_4_1
    libsoup_3
    librsvg
    # TLS for WebKit's own network stack.
    glib-networking
  ];

  desktopItems = [ desktop.desktopItem ];
  postInstall = desktop.installIcon;
  # Links and sign-in pages open through xdg-open (tauri-plugin-opener): the
  # user's own when there is one, this one otherwise.
  preFixup = ''
    gappsWrapperArgs+=(--suffix PATH : ${lib.makeBinPath [ xdg-utils ]})
  '';

  meta = {
    description = "A tutor that measures, plans, teaches and tracks what you can do";
    homepage = "https://github.com/DissatisfiedP/Ps-learning-harness";
    mainProgram = "ps-learning-harness";
    platforms = lib.platforms.linux;
  };
}

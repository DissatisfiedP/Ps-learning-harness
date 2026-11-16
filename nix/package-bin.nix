# The release's prebuilt Linux binary (built by CI on Ubuntu 22.04, glibc
# 2.35), linked against this nixpkgs' libraries: nothing is compiled, and it
# works whichever nixpkgs the user's configuration makes this flake follow.
# `release` is nix/bin.json, which the release workflow rewrites.
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  wrapGAppsHook3,
  copyDesktopItems,
  makeDesktopItem,
  release,
  openssl,
  zlib,
  dbus,
  gtk3,
  gdk-pixbuf,
  cairo,
  glib,
  webkitgtk_4_1,
  libsoup_3,
  glib-networking,
}:
let
  desktop = import ./desktop-item.nix { inherit makeDesktopItem; };
in
stdenv.mkDerivation {
  pname = "ps-learning-harness-bin";
  inherit (release) version;

  src = fetchurl { inherit (release) url hash; };
  sourceRoot = ".";

  nativeBuildInputs = [
    autoPatchelfHook
    wrapGAppsHook3
    copyDesktopItems
  ];
  # The binary's DT_NEEDED: OpenSSL 3, zlib, libstdc++, D-Bus, GTK 3 and
  # WebKitGTK 4.1 with libsoup 3. glib-networking gives WebKit TLS.
  buildInputs = [
    openssl
    zlib
    stdenv.cc.cc.lib
    dbus
    gtk3
    gdk-pixbuf
    cairo
    glib
    webkitgtk_4_1
    libsoup_3
    glib-networking
  ];

  desktopItems = [ desktop.desktopItem ];

  installPhase = ''
    runHook preInstall
    install -Dm755 ps-learning-harness $out/bin/ps-learning-harness
    ${desktop.installIcon}
    runHook postInstall
  '';

  meta = {
    description = "A tutor that measures, plans, teaches and tracks what you can do (prebuilt)";
    homepage = "https://github.com/DissatisfiedP/Ps-learning-harness";
    mainProgram = "ps-learning-harness";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}

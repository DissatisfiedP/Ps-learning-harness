{
  description = "P's Learning Harness — Tauri v2 app (Rust core, plain web UI) backed by plain files";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, fenix, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # The release CI writes after each GitHub release: where the prebuilt
      # Linux binary is and its hash. `null` until the first such release.
      binRelease = nixpkgs.lib.importJSON ./nix/bin.json;
    in
    {
      # `default` is the prebuilt release binary where there is one (x86_64,
      # a ~10 MB download, nothing compiled), else the source build.
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          source = pkgs.callPackage ./nix/package.nix { };
          bin = pkgs.callPackage ./nix/package-bin.nix { release = binRelease; };
          hasBin = binRelease != null && system == "x86_64-linux";
        in
        {
          inherit source;
          default = if hasBin then bin else source;
        }
        // nixpkgs.lib.optionalAttrs hasBin { inherit bin; }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config = {
              allowUnfree = true;
              android_sdk.accept_license = true;
            };
          };

          # Stable Rust from fenix (pinned by flake.lock). The Android shell adds
          # the std libraries for the four ABIs `cargo tauri android` builds.
          rust = fenix.packages.${system};
          components = with rust.stable; [
            cargo
            rustc
            clippy
            rustfmt
            rust-src
            rust-analyzer
          ];
          androidTargets = [
            "aarch64-linux-android"
            "armv7-linux-androideabi"
            "i686-linux-android"
            "x86_64-linux-android"
          ];
          hostToolchain = rust.combine components;
          androidToolchain = rust.combine (
            components ++ map (t: rust.targets.${t}.stable.rust-std) androidTargets
          );

          # The Tauri CLI and build tools. The UI is plain ES modules: no Node.
          tools = with pkgs; [
            cargo-tauri
            pkg-config
            git
          ];

          # Libraries Tauri v2 links on Linux (WebKitGTK 4.1 + libsoup 3).
          libs = with pkgs; [
            openssl
            # libgit2-sys links it; without it the crate compiles its own copy.
            zlib
            # Tauri's windowing crate (tao) links libdbus-1.
            dbus
            gtk3
            webkitgtk_4_1
            libsoup_3
            librsvg
            glib-networking
            # GSettings schemas GTK reads at runtime (not linked, but a devShell input).
            gsettings-desktop-schemas
          ];

          # GTK's file chooser reads `org.gtk.Settings.FileChooser` from the compiled
          # schemas of gtk3; with no XDG desktop portal, GLib aborts the process when
          # the schema is missing. The schema dirs are laid out NixOS-style under
          # <pkg>/share/gsettings-schemas/<pkg.name>/glib-2.0/schemas, and GTK also
          # reads the org.gnome.* neighbours shipped by gsettings-desktop-schemas.
          schemaDirs = map (p: "${p}/share/gsettings-schemas/${p.name}") (
            with pkgs;
            [
              gtk3
              gsettings-desktop-schemas
            ]
          );
          schemaHook = ''
            # GSETTINGS_SCHEMA_DIR replaces GLib's default search: list every dir.
            export GSETTINGS_SCHEMA_DIR="${pkgs.lib.concatStringsSep ":" (map (d: "${d}/glib-2.0/schemas") schemaDirs)}"
            # Prepend the same dirs for child processes; keep the inherited value.
            export XDG_DATA_DIRS="${pkgs.lib.concatStringsSep ":" schemaDirs}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
            # TLS for WebKit's own network stack (pictures over https): GIO finds
            # glib-networking only through this, and the session's own value, where
            # there is one, is for the system's GLib.
            export GIO_EXTRA_MODULES="${pkgs.glib-networking}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
          '';

          common = {
            buildInputs = libs;
            LANG = "C.UTF-8";
            LC_ALL = "C.UTF-8";
            shellHook = schemaHook;
          };

          # Android SDK + NDK for `cargo tauri android`; no emulator image.
          androidSdk =
            (pkgs.androidenv.composeAndroidPackages {
              platformVersions = [ "34" ];
              buildToolsVersions = [ "34.0.0" ];
              includeNDK = true;
              ndkVersion = "26.1.10909125";
              includeEmulator = false;
              includeSystemImages = false;
            }).androidsdk;
          sdkRoot = "${androidSdk}/libexec/android-sdk";
        in
        {
          default = pkgs.mkShell (common // { nativeBuildInputs = [ hostToolchain ] ++ tools; });

          android = pkgs.mkShell (
            common
            // {
              nativeBuildInputs = [ androidToolchain ] ++ tools ++ [
                androidSdk
                pkgs.jdk21
              ];
              JAVA_HOME = pkgs.jdk21.home;
              ANDROID_HOME = sdkRoot;
              ANDROID_SDK_ROOT = sdkRoot;
              ANDROID_NDK_ROOT = "${sdkRoot}/ndk-bundle";
              ANDROID_NDK_HOME = "${sdkRoot}/ndk-bundle";
              GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdkRoot}/build-tools/34.0.0/aapt2";
              shellHook = schemaHook + ''
                for d in "$ANDROID_HOME"/cmake/*/bin; do [ -d "$d" ] && export PATH="$d:$PATH"; done
              '';
            }
          );
        }
      );
    };
}

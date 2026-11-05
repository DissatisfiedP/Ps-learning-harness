{
  description = "Learning Harness — Tauri v2 app (Rust core, web UI) backed by plain files + git sync";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
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

          # Android SDK composition for the Tauri v2 mobile target.
          androidSdk = (
            pkgs.androidenv.composeAndroidPackages {
              platformVersions = [ "34" ];
              buildToolsVersions = [ "34.0.0" ];
              includeNDK = true;
              ndkVersion = "26.1.10909125";
              includeEmulator = false;
              includeSystemImages = false;
            }
          ).androidsdk;

          # Native tools: Rust core + shell. The UI is plain ES modules (no Node build).
          baseNative = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            git
            pkg-config
          ];

          # Tauri v2 Linux prerequisites: WebKitGTK + libsoup and friends (§6.1).
          baseBuild = with pkgs; [
            openssl
            gtk3
            webkitgtk_4_1
            libsoup_3
            librsvg
            libayatana-appindicator
            glib-networking
            libsecret
            xdo
          ];

          defaultHook = ''
            echo "learning-harness dev shell: $(rustc --version)"
          '';
        in
        {
          # Desktop + core work (build order steps 1–2).
          default = pkgs.mkShell {
            nativeBuildInputs = baseNative;
            buildInputs = baseBuild;
            LANG = "C.UTF-8";
            LC_ALL = "C.UTF-8";
            shellHook = defaultHook;
          };

          # Android: SDK + platform-tools + NDK + JDK. No emulator image.
          android = pkgs.mkShell {
            nativeBuildInputs = baseNative ++ [
              androidSdk
              pkgs.jdk21
            ];
            buildInputs = baseBuild;
            LANG = "C.UTF-8";
            LC_ALL = "C.UTF-8";
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";
            ANDROID_NDK_ROOT = "${androidSdk}/libexec/android-sdk/ndk-bundle";
            ANDROID_NDK_HOME = "${androidSdk}/libexec/android-sdk/ndk-bundle";
            GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${androidSdk}/libexec/android-sdk/build-tools/34.0.0/aapt2";
            shellHook = ''
              for d in "$ANDROID_HOME"/cmake/*/bin; do [ -d "$d" ] && export PATH="$d:$PATH"; done
            '' + defaultHook;
          };
        }
      );
    };
}

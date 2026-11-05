{
  description = "duet: read and listen to your books in sync";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };

          # Build in a persistent target directory (/var/tmp/cargo-nix, exposed to the sandbox by
          # the system flake) so rebuilds only recompile what changed, like nixos-update-checker
          # and Kog. Falls back to a throwaway directory when the sandbox doesn't expose it.
          # kache stays out of these builds: CARGO_HOME is the build's own.
          persistentCargo = name: cargoArgs: ''
            runHook preBuild
            workDir=$TMPDIR/cargo-nix
            if [ -w /var/tmp/cargo-nix ]; then
              workDir=/var/tmp/cargo-nix/${name}
            fi
            # The store vendor copy only changes with Cargo.lock; the per-build copy gets new
            # mtimes, which would rebuild every dependency.
            sed -i "s|^directory = .*|directory = \"${cargoDeps}\"|" "$NIX_BUILD_TOP/.cargo/config.toml"
            (
              # Everything stays writable by everyone: each build may run as a different user.
              # Kept to this subshell so the outputs get normal permissions.
              umask 000
              mkdir -p "$workDir/src" "$workDir/target"
              touch "$workDir"
              # Only rewrite files whose content changed; cargo's freshness check relies on mtimes.
              rsync -rc --no-times --no-perms --chmod=a+rwX --delete ./ "$workDir/src/"
              # The linker wrapper rejects output paths outside the build dir.
              export NIX_ENFORCE_PURITY=0
              cd "$workDir/src"
              CARGO_HOME="$NIX_BUILD_TOP/.cargo" CARGO_TARGET_DIR="$workDir/target" \
                cargo build ${cargoArgs} --offline --locked -j "$NIX_BUILD_CORES"
              find "$workDir/target" -mindepth 1 -user "$(id -u)" ! -perm -o+w -exec chmod a+rwX {} + 2>/dev/null || true
            )
            ln -sfn "$workDir/target" target
            runHook postBuild
          '';
        in
        rec {
          # The Leptos UI, compiled to WebAssembly.
          web = pkgs.rustPlatform.buildRustPackage {
            pname = "duet-web";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            inherit cargoDeps;
            nativeBuildInputs = [ pkgs.wasm-bindgen-cli pkgs.lld pkgs.rsync ];
            buildPhase = persistentCargo "duet-web"
              "--profile wasm-release --target wasm32-unknown-unknown -p duet-web";
            installPhase = ''
              runHook preInstall
              mkdir -p $out
              wasm-bindgen --target web --no-typescript --out-dir $out \
                target/wasm32-unknown-unknown/wasm-release/duet_web.wasm
              cp crates/web/static/* $out/
              runHook postInstall
            '';
            doCheck = false;
          };

          default = pkgs.rustPlatform.buildRustPackage {
            pname = "duet";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            inherit cargoDeps;
            nativeBuildInputs = [ pkgs.mold pkgs.rsync pkgs.makeWrapper ];
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
            buildPhase = persistentCargo "duet" "--release -p duet-server";
            installPhase = ''
              runHook preInstall
              install -Dm755 target/release/duet $out/bin/duet
              mkdir -p $out/share/duet
              ln -s ${web} $out/share/duet/web
              cp -r workers $out/share/duet/workers
              # Calibre converts MOBI/AZW3/FB2/DOCX; ffmpeg encodes generated audio.
              wrapProgram $out/bin/duet \
                --set-default DUET_WEB_DIR $out/share/duet/web \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.calibre pkgs.ffmpeg ]}
              runHook postInstall
            '';
            doCheck = false;
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ rustc cargo clippy rustfmt rust-analyzer wasm-bindgen-cli lld ffmpeg watchexec ];
        };
      });
    };
}

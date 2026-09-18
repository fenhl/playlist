{
    inputs = {
        flake.url = "github:fenhl/flake";
        rust-overlay = {
            url = "github:oxalica/rust-overlay";
            inputs.nixpkgs.follows = "flake/nixpkgs";
        };
    };
    outputs = attrs: attrs.flake.lib {
        overlays = [
            attrs.rust-overlay.overlays.default
        ];
        devShells.pre-commit = { pkgs, ... }: {
            packages = with pkgs; [
                cargo-deny
                rust-bin.nightly.latest.default # nightly cargo, required to run the pre-commit script
            ];
        };
        packages.default = { pkgs, ... }: let
            manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
        in pkgs.rustPlatform.buildRustPackage {
            inherit (manifest) version;
            pname = "playlist";
            cargoLock = {
                allowBuiltinFetchGit = true; # allows omitting cargoLock.outputHashes
                lockFile = ./Cargo.lock;
            };
            nativeBuildInputs = with pkgs; [
                installShellFiles # required for `installShellCompletion` in postInstall hook
            ];
            postInstall = let
                playlist = "${pkgs.stdenv.hostPlatform.emulator pkgs.buildPackages} $out/bin/playlist";
            in pkgs.lib.optionalString (pkgs.stdenv.hostPlatform.emulatorAvailable pkgs.buildPackages) ''
                installShellCompletion --cmd playlist \
                    --bash <(COMPLETE=bash ${playlist}) \
                    --fish <(COMPLETE=fish ${playlist}) \
                    --zsh <(COMPLETE=zsh ${playlist})
            '';
            src = ./.;
        };
    };
}

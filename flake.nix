{
  # The aviary images built by Nix (Deyao, 2026-11-26): each output is a pure function of its declared inputs, named by
  # their hash, so building is looking up that hash in a cache (the Hetzner bucket, `nixConfig` below) and building what
  # is absent. Parts move here one at a time, each beside its Dockerfile build until its Nix image passes the same image
  # checks in CI (.github/workflows/nix.yml). herald is the first.
  description = "aviary: images built from source by Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay, nix2container }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
      lib = pkgs.lib;
      n2c = nix2container.packages.${system}.nix2container;

      # rust-toolchain.toml, exactly (1.99.1, with its musl targets), as rustup gives CI
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

      # what cargo reads: the workspace's manifests and lock, its config, and the member crates (crates/, testing/)
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./rust-toolchain.toml
          ./.cargo
          ./crates
          ./testing
        ];
      };

      target = "x86_64-unknown-linux-musl";
      # static musl binaries as ci.yml's `build` makes them: rust-lld links (.cargo/config.toml), a musl C compiler
      # compiles the C in dependencies (ring). Only the musl target gets it (cargo reads CC_<target> before the config's
      # musl-gcc): build scripts run on the build machine with its own compiler, and musl's libraries in their link
      # left glibc's symbols undefined.
      muslCc = pkgs.pkgsCross.musl64.stdenv.cc;
      common = {
        inherit src;
        strictDeps = true;
        CARGO_BUILD_TARGET = target;
        CC_x86_64_unknown_linux_musl = "${muslCc}/bin/${muslCc.targetPrefix}cc";
        doCheck = false;
      };
      # a service's dependencies, compiled on their own (`-p`): what a change to the service's own code does not
      # rebuild, kept in the cache. One set per service, not the workspace's: the whole set compiles hermit's AWS SDK
      # (6.3 GB for one rustc) for every image.
      deps = name: craneLib.buildDepsOnly (common // {
        pname = "${name}-deps";
        version = "0.0.0";
        cargoExtraArgs = "--locked -p ${name}";
      });

      service = name: craneLib.buildPackage (common // {
        pname = name;
        version = "0.0.0";
        cargoArtifacts = deps name;
        cargoExtraArgs = "--locked -p ${name} --bins";
      });

      # gcr.io/distroless/static-debian12:nonroot, as the Dockerfiles start from: CA certificates, time zones, the
      # nonroot user and group 65532, a writable /tmp
      base = pkgs.runCommand "distroless-static" { } ''
        mkdir -p $out/etc $out/tmp $out/share
        chmod 1777 $out/tmp
        cp -rL ${pkgs.cacert}/etc/ssl $out/etc/ssl
        cp -rL ${pkgs.tzdata}/share/zoneinfo $out/share/zoneinfo
        echo 'root:x:0:0:root:/root:/sbin/nologin' > $out/etc/passwd
        echo 'nonroot:x:65532:65532:nonroot:/home/nonroot:/sbin/nologin' >> $out/etc/passwd
        echo 'root:x:0:' > $out/etc/group
        echo 'nonroot:x:65532:' >> $out/etc/group
      '';

      # an image as images/<name>/Dockerfile describes it: the binary at /usr/local/bin, the same labels, user, ports
      # and command. `revision` and `version` name the source by the hash Nix gives it, not by a commit: a commit in
      # the image would make every commit a new image, and no build would ever come from the cache.
      image = { name, bin, description, ports }: n2c.buildImage {
        inherit name;
        tag = "nix";
        # one root: two paths that both hold /etc are refused when one carries permissions (/tmp)
        copyToRoot = [
          base
          (pkgs.runCommand "${name}-bin" { } ''
            mkdir -p $out/usr/local/bin
            cp ${bin}/bin/${name} $out/usr/local/bin/${name}
          '')
        ];
        perms = [{ path = base; regex = "/tmp"; mode = "1777"; }];
        config = {
          User = "65532:65532";
          Entrypoint = [ "/usr/local/bin/${name}" ];
          Cmd = [ "serve" ];
          Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "TZDIR=/share/zoneinfo" ];
          ExposedPorts = lib.genAttrs (map (p: "${toString p}/tcp") ports) (_: { });
          Labels = {
            "org.opencontainers.image.source" = "https://github.com/DE0CH/aviary";
            "org.opencontainers.image.revision" = sourceHash;
            "org.opencontainers.image.version" = builtins.substring 0 12 sourceHash;
            "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
            "org.opencontainers.image.description" = description;
          };
        };
      };

      # the hash of the source's store path: it changes exactly when the source does
      sourceHash = builtins.substring 0 32 (baseNameOf src);
    in
    {
      packages.${system} = rec {
        # herald's compiled dependencies: kept in the cache, so a change to herald compiles only herald
        herald-deps = deps "herald";
        herald = service "herald";
        herald-image = image {
          name = "herald";
          bin = herald;
          description = "herald: Telegram notification channels for aviary conversations";
          ports = [ 8080 9090 ];
        };
        default = herald-image;
      };
    };
}

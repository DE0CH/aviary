{
  # The aviary images built by Nix (Deyao, 2026-11-26): each output is a pure function of its declared inputs, named by
  # their hash, so building is looking up that hash in a cache (the Hetzner bucket, `nixConfig` below) and building what
  # is absent. Parts move here one at a time, each beside its Dockerfile build until its Nix image passes the same image
  # checks in CI (.github/workflows/nix.yml). herald is the first.
  description = "aviary: images built from source by Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
      lib = pkgs.lib;

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
      deps = name: profile: craneLib.buildDepsOnly (common // {
        pname = "${name}-deps";
        version = "0.0.0";
        CARGO_PROFILE = profile;
        cargoExtraArgs = "--locked -p ${name}";
      });

      # `profile`: release, or sandboxd's own (Cargo.toml: it unwinds on a panic)
      serviceWith = name: profile: craneLib.buildPackage (common // {
        pname = name;
        version = "0.0.0";
        CARGO_PROFILE = profile;
        cargoArtifacts = deps name profile;
        cargoExtraArgs = "--locked -p ${name} --bins";
        # what `--version` and every log line's `ver` report (aviary-core version.rs): the source's hash, as the image's
        # labels say; on the service only, so a change to aviary's code leaves the cached dependencies valid
        AVIARY_REVISION = sourceHash;
      });
      service = name: serviceWith name "release";

      # gcr.io/distroless/static-debian12:nonroot, as the Dockerfiles start from: CA certificates, time zones, the
      # nonroot user and group 65532, a writable /tmp
      base = pkgs.runCommand "distroless-static" { } ''
        mkdir -p $out/etc $out/share $out/run $out/var
        # /var/run is /run, as on distroless and Alpine: iproute2 finds network namespaces in /var/run/netns
        ln -s /run $out/var/run
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
      image = { name, bin, description, ports ? [ ], user ? "65532:65532", cmd ? [ "serve" ], tools ? [ ], env ? [ ] }:
        pkgs.dockerTools.streamLayeredImage {
        inherit name;
        tag = "nix";
        # one root: the distroless files and the tools (linked from the store)
        contents = [ base ] ++ tools;
        # the binary as a real file at /usr/local/bin, as the Dockerfile puts it (a link into the store would be what
        # /proc/<pid>/exe names, and adopt.sh checks it is /usr/local/bin/sandboxd); /tmp writable by anyone, as
        # distroless has it (a store path is read-only)
        extraCommands = ''
          mkdir -p usr/local/bin tmp
          cp ${bin}/bin/${name} usr/local/bin/${name}
          chmod 755 usr/local/bin/${name}
          chmod 1777 tmp
        '';
        config = {
          User = user;
          Entrypoint = [ "/usr/local/bin/${name}" ];
          Cmd = cmd;
          Env = [ "PATH=/usr/local/bin:/bin:/sbin" "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "TZDIR=/share/zoneinfo" ] ++ env;
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
        herald-deps = deps "herald" "release";
        herald = service "herald";
        herald-image = image {
          name = "herald";
          bin = herald;
          description = "herald: Telegram notification channels for aviary conversations";
          ports = [ 8080 9090 ];
        };
        pigeon-deps = deps "pigeon" "release";
        pigeon = service "pigeon";
        pigeon-image = image {
          name = "pigeon";
          bin = pigeon;
          description = "pigeon: the app-facing gateway of aviary";
          ports = [ 8080 9090 ];
        };
        sandboxd-deps = deps "sandboxd" "sandboxd";
        sandboxd = serviceWith "sandboxd" "sandboxd";
        # images/sandboxd/Dockerfile: the gateway pod's image, as root, with the commands sandboxd runs on a gateway
        # (crane: nixpkgs' 0.21.6, where the Dockerfile pins 0.20.6), and a shell for the image check
        sandboxd-image = image {
          name = "sandboxd";
          bin = sandboxd;
          description = "sandboxd: hermit's sandbox host agent, run here as the fabric's gateway";
          user = "0:0";
          cmd = [ ];
          # pkgs.crane by name: inside `with pkgs`, `crane` is this flake's crane input (`with` never shadows)
          tools = with pkgs; [ iproute2 nftables wireguard-tools procps gnutar zstd pkgs.crane.crane bash coreutils ];
          env = [ "SBX_CONFIG=/etc/sbx/host.json" ];
        };
        default = herald-image;
      };
    };
}

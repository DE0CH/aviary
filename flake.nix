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

      # images/aviary-testing: the test binaries, one build (their dependencies built once for all of them)
      testingCrates = [ "aviary-conformance" "test-gateway" "mock-model" "fake-tools" "fake-access" "fake-sandboxd" "aviary-e2e" ];
      testingArgs = "--locked " + lib.concatMapStringsSep " " (c: "-p ${c}") testingCrates;
      aviary-testing-deps = craneLib.buildDepsOnly (common // {
        pname = "aviary-testing-deps";
        version = "0.0.0";
        cargoExtraArgs = testingArgs;
      });
      aviary-testing = craneLib.buildPackage (common // {
        pname = "aviary-testing";
        version = "0.0.0";
        cargoArtifacts = aviary-testing-deps;
        cargoExtraArgs = testingArgs + " --bins";
        AVIARY_REVISION = sourceHash;
      });
      aviary-testing-image = pkgs.dockerTools.streamLayeredImage {
        name = "aviary-testing";
        tag = "nix";
        contents = [ base aviary-testing ];
        extraCommands = ''
          mkdir -p usr/local/bin tmp
          ${lib.concatMapStrings (c: "cp ${aviary-testing}/bin/${c} usr/local/bin/${c}\n") testingCrates}
          chmod 755 usr/local/bin/*
          chmod 1777 tmp
        '';
        config = {
          User = "65532:65532";
          Env = [ "PATH=/usr/local/bin:/bin" "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "TZDIR=/share/zoneinfo" ];
          Labels = {
            "org.opencontainers.image.source" = "https://github.com/DE0CH/aviary";
            "org.opencontainers.image.revision" = sourceHash;
            "org.opencontainers.image.version" = builtins.substring 0 12 sourceHash;
            "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
            "org.opencontainers.image.description" = "aviary's test binaries: conformance suites, reference gateway, fakes";
          };
        };
      };

      # magpie: glibc, not musl, with libheif (and its HEVC decoder) as .github/ci/build-magpie.sh builds it on Debian
      magpieCommon = {
        inherit src;
        strictDeps = true;
        nativeBuildInputs = [ pkgs.pkg-config pkgs.rustPlatform.bindgenHook ];
        buildInputs = [ pkgs.libheif ];
        doCheck = false;
      };
      magpie-deps = craneLib.buildDepsOnly (magpieCommon // {
        pname = "magpie-deps";
        version = "0.0.0";
        cargoExtraArgs = "--locked -p magpie";
      });
      magpie = craneLib.buildPackage (magpieCommon // {
        pname = "magpie";
        version = "0.0.0";
        cargoArtifacts = magpie-deps;
        cargoExtraArgs = "--locked -p magpie --bin magpie";
        AVIARY_REVISION = sourceHash;
      });

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
      image = { name, bin, description, ports ? [ ], user ? "65532:65532", cmd ? [ "serve" ], tools ? [ ], env ? [ ],
                extraBins ? [ ] }:
        pkgs.dockerTools.streamLayeredImage {
        inherit name;
        tag = "nix";
        # one root: the distroless files, the tools, and the service's package (linked from the store; it brings
        # the libraries a dynamically linked binary loads, magpie's libheif)
        contents = [ base bin ] ++ tools;
        # the binary as a real file at /usr/local/bin, as the Dockerfile puts it (a link into the store would be what
        # /proc/<pid>/exe names, and adopt.sh checks it is /usr/local/bin/sandboxd); /tmp writable by anyone, as
        # distroless has it (a store path is read-only)
        extraCommands = ''
          mkdir -p usr/local/bin tmp
          cp ${bin}/bin/${name} usr/local/bin/${name}
          ${lib.concatMapStrings (b: "cp ${b}/bin/* usr/local/bin/\n") extraBins}
          chmod 755 usr/local/bin/*
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

      # images/hermit/Dockerfile's juicefs: the same release, checked by the same sha256 (a fetch is the one step whose
      # answer may vary; its bytes may not)
      juicefs = pkgs.runCommand "juicefs-1.4.1" {
        src = pkgs.fetchurl {
          url = "https://github.com/juicedata/juicefs/releases/download/v1.4.1/juicefs-1.4.1-linux-amd64.tar.gz";
          sha256 = "01ee09a21a9351a465e09906f113845e1c6a19bea70f530e5e2b2125b2dd3b82";
        };
      } ''
        mkdir -p $out/bin
        tar -xzf $src -C $out/bin juicefs
        chmod 755 $out/bin/juicefs
      '';

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
        inherit juicefs;
        hermit-deps = deps "hermit" "release";
        hermit = service "hermit";
        hermit-image = image {
          name = "hermit";
          bin = hermit;
          description = "hermit: sandboxes, people and workspaces";
          extraBins = [ juicefs ];
        };
        inherit magpie-deps magpie aviary-testing-deps aviary-testing aviary-testing-image;
        magpie-image = image {
          name = "magpie";
          bin = magpie;
          description = "magpie: conversations, timelines and blobs of aviary";
          ports = [ 8080 9090 ];
          user = "10001:10001";
        };
        default = herald-image;
      };
    };
}

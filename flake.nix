{
  # The aviary images built by Nix (Deyao, 2026-11-26): each output is a pure function of its declared inputs, named by
  # their hash, so building is looking up that hash in a cache (the Hetzner bucket, .github/workflows/nix.yml) and
  # building what is absent. Parts move here one at a time, each beside its Dockerfile build until its Nix image passes
  # the same image checks in CI (.github/workflows/nix.yml). Every image is built for x86_64-linux and aarch64-linux,
  # each natively on a machine of its architecture, as ci.yml's `images` builds them.
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
      systems = [ "x86_64-linux" "aarch64-linux" ];

      perSystem = system:
        let
          pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
          lib = pkgs.lib;
          arch = { x86_64-linux = "x86_64"; aarch64-linux = "aarch64"; }.${system};
          # Go's and Docker's name for it (juicefs' release, the image's architecture)
          goArch = { x86_64-linux = "amd64"; aarch64-linux = "arm64"; }.${system};

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

          # What `--version`, every log line's `ver` (aviary-core version.rs) and the image's labels name: the part's
          # commit when CI passes it in (AVIARY_REVISION, read by an `--impure` build: deploys pin `<name>:<sha12>` and
          # production is read by it, lesson 111), else the source's hash (a pure build). The commit is the last one to
          # change the part's files, so it changes when the source does; it is given to the service only, never to its
          # dependencies, which stay cached across commits.
          revisionFrom = var: fallback: let r = builtins.getEnv var; in if r != "" then r else fallback;
          revision = revisionFrom "AVIARY_REVISION" sourceHash;

          target = "${arch}-unknown-linux-musl";
          # static musl binaries as ci.yml's `build` makes them: rust-lld links (.cargo/config.toml), a musl C compiler
          # compiles the C in dependencies (ring). Only the musl target gets it (cargo reads CC_<target> before the
          # config's musl-gcc): build scripts run on the build machine with its own compiler, and musl's libraries in
          # their link left glibc's symbols undefined.
          muslCc = { x86_64-linux = pkgs.pkgsCross.musl64; aarch64-linux = pkgs.pkgsCross.aarch64-multiplatform-musl; }
            .${system}.stdenv.cc;
          common = {
            inherit src;
            strictDeps = true;
            CARGO_BUILD_TARGET = target;
            "CC_${arch}_unknown_linux_musl" = "${muslCc}/bin/${muslCc.targetPrefix}cc";
            doCheck = false;
          };
          # a service's dependencies, compiled on their own (`-p`): what a change to the service's own code does not
          # rebuild, kept in the cache. One set per service, not the workspace's: the whole set compiles hermit's AWS
          # SDK (6.3 GB for one rustc) for every image.
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
            AVIARY_REVISION = revision;
          });
          service = name: serviceWith name "release";

          # platform 5.3's labels, as images/build.sh gives them
          ociLabels = rev: description: {
            "org.opencontainers.image.source" = "https://github.com/DE0CH/aviary";
            "org.opencontainers.image.revision" = rev;
            "org.opencontainers.image.version" = builtins.substring 0 12 rev;
            "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
            "org.opencontainers.image.description" = description;
          };

          # The provides/accepts labels from the built binary's own `--version` line (`<name> <sha12> protocols=<json>`),
          # as images/build.sh reads them (jq -c, so the label's bytes are the same), so a label never disagrees with
          # the code. Read when the image is evaluated (the binary is built first).
          protocolLabels = name: bin:
            let
              out = pkgs.runCommand "${name}-protocols" { nativeBuildInputs = [ pkgs.jq ]; } ''
                line=$(${bin}/bin/${name} --version)
                protocols=''${line#* protocols=}
                [ "$protocols" != "$line" ] || { echo "'$line' has no protocols=" >&2; exit 1; }
                mkdir $out
                jq -j -c .provides <<<"$protocols" > $out/provides
                jq -j -c .accepts <<<"$protocols" > $out/accepts
              '';
            in {
              "dev.de0ch.aviary.provides" = builtins.readFile "${out}/provides";
              "dev.de0ch.aviary.accepts" = builtins.readFile "${out}/accepts";
            };
          noProtocols = { "dev.de0ch.aviary.provides" = "{}"; "dev.de0ch.aviary.accepts" = "{}"; };

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
            AVIARY_REVISION = revision;
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
              Labels = ociLabels revision "aviary's test binaries: conformance suites, reference gateway, fakes"
                // noProtocols;
            };
          };

          # jarvis/hermit: hermit-jarvis, the Jarvis chat's hermit, a workspace of its own (its Cargo.lock) using
          # aviary's crates by path, which inherit from the root workspace: glibc, as its Dockerfile builds it on Debian
          jarvisSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./rust-toolchain.toml ./.cargo ./crates ./testing ./jarvis/hermit ];
          };
          jarvisCommon = {
            src = jarvisSrc;
            cargoToml = ./jarvis/hermit/Cargo.toml;
            cargoLock = ./jarvis/hermit/Cargo.lock;
            postUnpack = ''
              cd $sourceRoot/jarvis/hermit
              sourceRoot=.
            '';
            strictDeps = true;
            doCheck = false;
          };
          hermit-jarvis-deps = craneLib.buildDepsOnly (jarvisCommon // {
            pname = "hermit-jarvis-deps";
            version = "0.0.0";
            cargoExtraArgs = "--locked --bin hermit-jarvis";
          });
          # jarvis.yml passes its part's commit as AVIARY_REVISION too
          jarvisRevision = revisionFrom "AVIARY_REVISION" (builtins.substring 0 32 (baseNameOf jarvisSrc));
          hermit-jarvis = craneLib.buildPackage (jarvisCommon // {
            pname = "hermit-jarvis";
            version = "0.0.0";
            cargoArtifacts = hermit-jarvis-deps;
            cargoExtraArgs = "--locked --bin hermit-jarvis";
            AVIARY_REVISION = jarvisRevision;
          });

          # egress/Dockerfile: the TypeScript run by Node 22 itself, its production dependencies (npm ci --omit=dev,
          # pinned by the lock file's hash), tini, as user 1000 with a writable /var/cache/jarvis-egress. CHAT_COMMIT
          # is the part's commit when egress.yml passes it in, else the source's hash; CHAT_BUILT_AT stays empty (the
          # Dockerfile's default), since a build time makes every build differ.
          egress-app = pkgs.buildNpmPackage {
            pname = "egress";
            version = "0.0.0";
            src = lib.fileset.toSource {
              root = ./egress;
              fileset = lib.fileset.unions [
                ./egress/package.json ./egress/package-lock.json ./egress/src
                ./egress/egress-allowlist/allow.txt ./egress/egress-allowlist/deny.txt
              ];
            };
            npmDepsHash = "sha256-FzT4oen4r3zydxYIPqYGzRJdZ+xZBCa5UK/1KX2149A=";
            nodejs = pkgs.nodejs_22;
            dontNpmBuild = true;
            installPhase = ''
              runHook preInstall
              npm prune --omit=dev --no-audit --no-fund
              mkdir -p $out/app
              cp -r package.json node_modules src egress-allowlist $out/app/
              runHook postInstall
            '';
          };
          egressRevision = revisionFrom "CHAT_COMMIT" (builtins.substring 0 32 (baseNameOf egress-app));
          egress-image = pkgs.dockerTools.streamLayeredImage {
            name = "egress";
            tag = "nix";
            contents = [ base egress-app pkgs.nodejs_22 pkgs.tini pkgs.bash pkgs.coreutils ];
            fakeRootCommands = ''
              mkdir -p var/cache/jarvis-egress tmp
              chown -R 1000:1000 var/cache/jarvis-egress
              chmod 1777 tmp
            '';
            enableFakechroot = false;
            config = {
              User = "1000:1000";
              WorkingDir = "/app";
              Entrypoint = [ "/bin/tini" "--" "/bin/node" "--disable-warning=ExperimentalWarning" "src/main.ts" ];
              Env = [
                "PATH=/bin" "NODE_ENV=production" "PORT=8080" "CHAT_COMMIT=${egressRevision}" "CHAT_BUILT_AT="
                "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-bundle.crt"
              ];
              ExposedPorts = lib.genAttrs [ "8080/tcp" "3128/tcp" "8082/tcp" "9090/tcp" ] (_: { });
              Labels = ociLabels egressRevision "egress: the sandboxes' gateway out, and tools"
                // { "jarvis3.chat-commit" = egressRevision; };
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
            AVIARY_REVISION = revision;
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

          # an image as images/<name>/Dockerfile describes it: the binary at /usr/local/bin, the same labels, user,
          # ports and command. `protocols`: the labels from the binary's `--version` (true), `{}` (false), or none
          # (null: hermit-jarvis, whose workflow gives none).
          image = { name, bin, description, ports ? [ ], user ? "65532:65532", cmd ? [ "serve" ], tools ? [ ], env ? [ ],
                    extraBins ? [ ], protocols ? false, rev ? revision }:
            pkgs.dockerTools.streamLayeredImage {
            inherit name;
            tag = "nix";
            # one root: the distroless files, the tools, and the service's package (linked from the store; it brings
            # the libraries a dynamically linked binary loads, magpie's libheif)
            contents = [ base bin ] ++ tools;
            # the binary as a real file at /usr/local/bin, as the Dockerfile puts it (a link into the store would be
            # what /proc/<pid>/exe names, and adopt.sh checks it is /usr/local/bin/sandboxd); /tmp writable by anyone,
            # as distroless has it (a store path is read-only)
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
              Labels = ociLabels rev description
                // (if protocols == true then protocolLabels name bin else if protocols == false then noProtocols else { });
            };
          };

          # images/hermit/Dockerfile's juicefs: the same release, checked by the same sha256 (a fetch is the one step
          # whose answer may vary; its bytes may not)
          juicefs = pkgs.runCommand "juicefs-1.4.1" {
            src = pkgs.fetchurl {
              url = "https://github.com/juicedata/juicefs/releases/download/v1.4.1/juicefs-1.4.1-linux-${goArch}.tar.gz";
              sha256 = {
                amd64 = "01ee09a21a9351a465e09906f113845e1c6a19bea70f530e5e2b2125b2dd3b82";
                arm64 = "1015ade83a7a93180a29f6c93ee5780a3eda52522331934ef2ef0cc0921995fd";
              }.${goArch};
            };
          } ''
            mkdir -p $out/bin
            tar -xzf $src -C $out/bin juicefs
            chmod 755 $out/bin/juicefs
          '';

          # the hash of the source's store path: it changes exactly when the source does
          sourceHash = builtins.substring 0 32 (baseNameOf src);
        in
        rec {
          # herald's compiled dependencies: kept in the cache, so a change to herald compiles only herald
          herald-deps = deps "herald" "release";
          herald = service "herald";
          herald-image = image {
            name = "herald";
            bin = herald;
            description = "herald: Telegram notification channels for aviary conversations";
            ports = [ 8080 9090 ];
            protocols = true;
          };
          pigeon-deps = deps "pigeon" "release";
          pigeon = service "pigeon";
          pigeon-image = image {
            name = "pigeon";
            bin = pigeon;
            description = "pigeon: the app-facing gateway of aviary";
            ports = [ 8080 9090 ];
            protocols = true;
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
            protocols = true;
          };
          inherit egress-app egress-image;
          inherit magpie-deps magpie aviary-testing-deps aviary-testing aviary-testing-image hermit-jarvis-deps hermit-jarvis;
          # jarvis/hermit/Dockerfile: distroless cc (glibc), an entrypoint and no command
          hermit-jarvis-image = image {
            name = "hermit-jarvis";
            bin = hermit-jarvis;
            description = "hermit-jarvis: the Jarvis chat's hermit";
            cmd = [ ];
            protocols = null;
            rev = jarvisRevision;
          };
          magpie-image = image {
            name = "magpie";
            bin = magpie;
            description = "magpie: conversations, timelines and blobs of aviary";
            ports = [ 8080 9090 ];
            user = "10001:10001";
            protocols = true;
          };
          default = herald-image;
        };
    in
    {
      packages = nixpkgs.lib.genAttrs systems perSystem;
    };
}

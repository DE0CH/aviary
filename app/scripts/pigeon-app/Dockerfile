# syntax=docker/dockerfile:1.7
# ghcr.io/de0ch/pigeon-app:<commit 12> (pigeon spec 17.1 step 8): the Paseo web app's files under /app and a static
# /cp, nothing else. pigeon's initContainer runs `/cp -a /app/. /srv/app/` into an emptyDir that pigeon serves. /meta
# holds the asset history the next build merges from (scripts/pigeon-app/assemble.mjs); it is not copied, so not
# served. Built by .github/workflows/pigeon-app.yml; build context: the assembled directory (app/, meta/).
FROM busybox:1.37.0-musl@sha256:f6a95d611e0a8780fabdb2aec43e31275b2720dfefa3e41b16835bfc42454478 AS busybox

FROM scratch
# busybox picks the applet from its name: /cp is busybox's cp (static, musl)
COPY --from=busybox /bin/busybox /cp
# owned by pigeon's uid, so `cp -a` run as that uid keeps owner and modes without an error
COPY --chown=65532:65532 app /app
COPY --chown=65532:65532 meta /meta
USER 65532:65532

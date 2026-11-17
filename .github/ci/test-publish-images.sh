#!/usr/bin/env bash
# .github/ci/publish-images.sh against a stand-in docker: a push the registry fails once ("unknown blob", GHCR
# 2026-11-17: no release for that commit) is pushed again and the release gets its digest; a push that keeps failing
# fails the script; every image's digest lands in the digests file.
#   bash .github/ci/test-publish-images.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/amd64" "$tmp/arm64" "$tmp/state"
for n in pigeon magpie hermit herald sandboxd aviary-testing hermit-sandbox; do echo x > "$tmp/amd64/$n.tar"; done
for n in pigeon magpie hermit herald sandboxd aviary-testing; do echo x > "$tmp/arm64/$n.tar"; done
cat > "$tmp/bin/docker" <<'EOF'
#!/usr/bin/env bash
# stand-in docker: S is the state dir; FAILS is how many pushes of hermit-sandbox the registry fails
set -euo pipefail
S=$STATE
dig() { printf 'sha256:%064d' "$(cksum <<< "$1" | cut -d' ' -f1)"; }
case "$1 ${2:-}" in
  "load -q") case "$4" in */arm64/*) echo arm64 > "$S/arch";; *) echo amd64 > "$S/arch";; esac ;;
  "tag "*|"rmi "*) ;;
  "image inspect") echo "linux/$(cat "$S/arch")" ;;
  "push "*)
    echo "$2" >> "$S/pushes"
    if [[ $2 == *hermit-sandbox* ]] && [ "$(grep -c hermit-sandbox "$S/pushes")" -le "$FAILS" ]; then
      echo "554b489923db: Pushing"; echo "unknown blob"; exit 1
    fi
    echo "${2##*:}: digest: $(dig "$2") size: 3023" ;;
  "buildx imagetools")
    if [ "$3" = create ]; then echo "$5" >> "$S/indexes"; exit 0; fi
    grep -qxF "$4" "$S/indexes" 2>/dev/null || exit 1
    [ "${5:-}" = --format ] && echo "\"$(dig "$4")\"" || echo "Name: $4" ;;
  *) echo "stand-in docker: unexpected: $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/docker"
export PATH="$tmp/bin:$PATH" STATE="$tmp/state" PUBLISH_PUSH_WAIT=0
fail() { echo "FAIL: $*" >&2; exit 1; }
run() { rm -f "$STATE"/*; FAILS=$1 bash "$here/publish-images.sh" "$tmp/amd64" "$tmp/arm64" abcdef012345 "$tmp/d.json" \
  > "$tmp/out" 2>&1; }

run 1 || fail "one failed push of hermit-sandbox: $(cat "$tmp/out")"
[ "$(grep -c 'hermit-sandbox:abcdef012345$' "$STATE/pushes")" = 2 ] || fail "hermit-sandbox pushed $(grep -c hermit-sandbox "$STATE/pushes") times, want 2"
for n in pigeon magpie hermit herald sandboxd aviary-testing hermit-sandbox; do
  jq -e --arg n "$n" '.[$n] | test("^sha256:[0-9]{64}$")' "$tmp/d.json" >/dev/null || fail "no digest for $n: $(cat "$tmp/d.json")"
done

if run 9; then fail "a push that keeps failing passed"; fi
grep -q "no digest for ghcr.io/de0ch/hermit-sandbox:abcdef012345" "$tmp/out" || fail "a push that keeps failing says so: $(cat "$tmp/out")"
echo "publish-images: ok"

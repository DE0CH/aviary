#!/usr/bin/env bash
# .github/ci/pin-dispatch.sh against a stand-in gh: every target of PIN_DISPATCH is dispatched on main, comments and
# blank lines are skipped, no targets is a no-op, and a malformed line or a missing token fails before any dispatch.
#   bash .github/ci/test-pin-dispatch.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
echo "token=$GH_TOKEN $*" >> "$GH_LOG"
EOF
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH" GH_LOG="$tmp/gh.log"
fail() { echo "FAIL: $*" >&2; exit 1; }
run() { : > "$GH_LOG"; env "$@" bash "$here/pin-dispatch.sh" > "$tmp/out" 2>&1; }

run GH_TOKEN=t1 PIN_DISPATCH=$'# deployments that pin aviary\nOWNER/one pin.yml\n\n  OWNER/two other-pin.yml  \n' \
  || fail "two targets: $(cat "$tmp/out")"
[ "$(cat "$GH_LOG")" = $'token=t1 workflow run pin.yml -R OWNER/one --ref main\ntoken=t1 workflow run other-pin.yml -R OWNER/two --ref main' ] \
  || fail "two targets dispatched: $(cat "$GH_LOG")"

run GH_TOKEN= PIN_DISPATCH= || fail "no targets: $(cat "$tmp/out")"
[ ! -s "$GH_LOG" ] || fail "no targets dispatched something: $(cat "$GH_LOG")"
grep -q "no deployment to dispatch" "$tmp/out" || fail "no targets says so: $(cat "$tmp/out")"

if run GH_TOKEN= PIN_DISPATCH='OWNER/one pin.yml'; then fail "no token passed"; fi
[ ! -s "$GH_LOG" ] || fail "no token dispatched: $(cat "$GH_LOG")"
grep -q PIN_DISPATCH_TOKEN "$tmp/out" || fail "no token names the secret: $(cat "$tmp/out")"

for bad in 'OWNER/one' 'one pin.yml' 'OWNER/one pin.yml extra' 'OWNER/one ../pin.yml'; do
  if run GH_TOKEN=t1 PIN_DISPATCH=$'OWNER/ok pin.yml\n'"$bad"; then fail "malformed line passed: $bad"; fi
  [ ! -s "$GH_LOG" ] || fail "malformed line ($bad) dispatched: $(cat "$GH_LOG")"
done
echo "pin-dispatch: ok"

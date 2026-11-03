#!/usr/bin/env bash
# Offline test of legacy-down.sh: stand-ins for aws, systemctl, ip, nft and curl record every call and answer from
# canned values. Checks that every name comes from the --config file, that a dry run changes nothing, that --confirm
# makes exactly the teardown calls, that empty tag sets are refused, and that a path with a mount under it is never
# removed. Needs bash and jq. Usage: test-legacy-down.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
mkdir "$t/bin"
for tool in aws systemctl ip nft curl; do
  cat > "$t/bin/$tool" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "$tool" "\$*" >> "\$CALLS"
args=" \$* "
case "$tool" in
  aws)
    case \$args in
      *" describe-instances "*) echo "i-01 i-02" ;;
      *" describe-launch-templates "*) exit 0 ;;
      *" describe-images "*"--owners self"*) echo ami-0old ;;
      *" describe-images "*"--image-ids ami-0old"*) echo snap-01 ;;
      *" iam get-user "*"--user-name old-scaler"*) exit 0 ;;
      *" iam get-user "*) exit 254 ;;
      *" list-access-keys "*) echo AKIA0TEST ;;
      *" list-user-policies "*) echo inline-policy ;;
      *" list-attached-user-policies "*) echo None ;;
      *" describe-security-groups "*) echo sg-0old ;;
    esac ;;
  systemctl) case \$args in *" cat old-sandboxd "*) exit 0 ;; *" cat "*) exit 1 ;; esac ;;
  ip)
    case \$args in
      *" link show wg9 "*) exit 0 ;;
      *" rule show "*) printf '0:\tfrom all lookup local\n32765:\tfrom all iif wg9 lookup 51899\n32766:\tfrom all lookup main\n' ;;
      *" route show table 51899 "*) echo "10.244.0.0/16 via 10.0.0.1 dev enp7s0" ;;
    esac ;;
  nft) exit 0 ;;
  curl)
    case \$args in
      *"/networks?name=test-net"*) echo '{"networks":[{"id":7,"routes":[{"destination":"10.200.0.0/15","gateway":"10.0.0.6"},{"destination":"10.9.0.0/16","gateway":"10.0.0.2"}]}]}' ;;
      *"/firewalls?name=test-fw"*) echo '{"firewalls":[{"id":9,"applied_to":[{"type":"server","server":{"id":42}}]}]}' ;;
      *"/firewalls/9"*) echo '{"firewall":{"applied_to":[]}}' ;;
    esac ;;
esac
EOF
  chmod +x "$t/bin/$tool"
done
mkdir -p "$t/etc-old" "$t/lib-old/jfs" "$t/lib-old/images"
touch "$t/lib-old/jfs/workspace-file"
cat > "$t/legacy.json" <<EOF
{"aws": {"region": "xx-test-1", "instanceTags": {"app": "old-host", "deployment": "example"}, "launchTemplate": "old-template",
         "amiTags": {"app": "old-host"}, "securityGroup": "old-sg", "iamUsers": ["old-scaler", "old-ami-ci"]},
 "host": {"units": ["old-sandboxd", "old-jfs"], "wgInterface": "wg9", "nftTable": "ip oldtable", "routeTable": 51899,
          "paths": ["$t/etc-old", "$t/lib-old"]},
 "hetzner": {"network": "test-net", "routes": ["10.200.0.0/15", "10.250.0.0/16"], "firewall": "test-fw"}}
EOF
: > "$t/mountinfo"
export CALLS=$t/calls PATH=$t/bin:$PATH HCLOUD_TOKEN=test-token HCLOUD_API=https://hc.test/v1 MOUNTINFO=$t/mountinfo
fail() { echo "FAIL: $*" >&2; cat "$CALLS" >&2 2>/dev/null || true; exit 1; }
run() { : > "$CALLS"; bash "$here/legacy-down.sh" --config "$t/legacy.json" "$@" > "$t/out" 2>&1; }
has() { grep -qF -- "$1" "$CALLS" || fail "no call with: $1"; }
hasnt() { if grep -qF -- "$1" "$CALLS"; then fail "a call with: $1"; fi; }

if bash "$here/legacy-down.sh" aws >/dev/null 2>&1; then fail "runs without --config"; fi
if bash "$here/legacy-down.sh" --config "$t/legacy.json" >/dev/null 2>&1; then fail "runs without a step"; fi

# dry runs: reads only
for s in aws host hetzner; do
  run "$s" || fail "dry run $s: $(cat "$t/out")"
  grep -q "^dry run" "$t/out" || fail "dry run $s does not say so"
  for verb in terminate-instances delete-launch-template deregister-image delete-snapshot delete-access-key \
      delete-user-policy delete-user delete-security-group "disable --now" "link del" "delete table" "rule del" \
      "route flush" delete_route remove_from_resources "-X DELETE"; do
    hasnt "$verb"
  done
done
[ -e "$t/etc-old" ] || fail "dry run removed a path"
echo "ok  a dry run of every step only reads"

run aws --confirm || fail "aws: $(cat "$t/out")"
has "aws --region xx-test-1 ec2 describe-instances --filters Name=tag:app,Values=old-host Name=tag:deployment,Values=example"
has "ec2 terminate-instances --instance-ids i-01 i-02"
has "ec2 wait instance-terminated --instance-ids i-01 i-02"
has "ec2 delete-launch-template --launch-template-name old-template"
has "ec2 describe-images --owners self --filters Name=tag:app,Values=old-host"
has "ec2 deregister-image --image-id ami-0old"
has "ec2 delete-snapshot --snapshot-id snap-01"
has "iam delete-access-key --user-name old-scaler --access-key-id AKIA0TEST"
has "iam delete-user-policy --user-name old-scaler --policy-name inline-policy"
has "iam delete-user --user-name old-scaler"
hasnt "delete-user --user-name old-ami-ci"
has "ec2 delete-security-group --group-id sg-0old"
# the security group goes after the instances are gone
[ "$(grep -n 'wait instance-terminated' "$CALLS" | cut -d: -f1)" -lt "$(grep -n 'delete-security-group' "$CALLS" | cut -d: -f1)" ] \
  || fail "security group deleted before the instances are terminated"
echo "ok  aws: tagged instances (every tag), template, tagged AMIs and snapshots, IAM users, security group"

jq '.aws.instanceTags = {}' "$t/legacy.json" > "$t/empty.json"
: > "$CALLS"
if bash "$here/legacy-down.sh" --config "$t/empty.json" aws --confirm > "$t/out" 2>&1; then fail "runs with no instance tags"; fi
grep -q "refusing to match every instance" "$t/out" || fail "empty tags: $(cat "$t/out")"
hasnt terminate-instances
echo "ok  aws: empty tag sets are refused"

# a workspace mount under a path: that path is refused and kept, the others still go
printf '36 25 0:52 / %s/jfs rw - fuse.juicefs JuiceFS:x rw\n' "$t/lib-old" > "$t/mountinfo"
if run host --confirm; then fail "host passes with a mount under a path"; fi
grep -q "refused: something is mounted at or under $t/lib-old" "$t/out" || fail "no refusal: $(cat "$t/out")"
[ -e "$t/lib-old/jfs/workspace-file" ] || fail "a path with a mount under it was removed"
[ ! -e "$t/etc-old" ] || fail "the other path was kept"
has "systemctl disable --now old-sandboxd"
hasnt "disable --now old-jfs"
has "ip link del wg9"
has "nft delete table ip oldtable"
has "ip rule del priority 32765"
hasnt "rule del priority 32766"
has "ip route flush table 51899"
: > "$t/mountinfo"
run host --confirm || fail "host: $(cat "$t/out")"
[ ! -e "$t/lib-old" ] || fail "the path was kept with nothing mounted"
echo "ok  host: units, interface, nft table, the table's rules and routes, paths (never one with a mount under it)"

jq --arg p "/var/lib" '.host.paths = [$p]' "$t/legacy.json" > "$t/root.json"
: > "$t/mountinfo"
if bash "$here/legacy-down.sh" --config "$t/root.json" host --confirm > "$t/out" 2>&1; then fail "removes /var/lib"; fi
grep -q "refused: /var/lib is not a fabric path" "$t/out" || fail "no refusal: $(cat "$t/out")"
echo "ok  host: system paths are refused"

run hetzner --confirm || fail "hetzner: $(cat "$t/out")"
has 'https://hc.test/v1/networks/7/actions/delete_route -d {"destination":"10.200.0.0/15","gateway":"10.0.0.6"}'
hasnt "10.9.0.0/16"
grep -q "ok: no route 10.250.0.0/16 on test-net" "$t/out" || fail "missing route not skipped"
has 'https://hc.test/v1/firewalls/9/actions/remove_from_resources -d {"remove_from": [{"type":"server","server":{"id":42}}]}'
has "-X DELETE https://hc.test/v1/firewalls/9"
hasnt "test-token"
echo "ok  hetzner: the configured routes and the firewall (detached first); the token never on a command line"

#!/usr/bin/env bash
# Teardown of a deployment's previous sandbox fabric, once, after the switch to hermit (hermit spec 22.3 step 5; the
# switch's A6). Every name comes from the deployment's legacy file (--config, JSON; nothing here names a deployment):
#
#   {"aws":     {"region": "…", "instanceTags": {"k": "v", …}, "launchTemplate": "…", "amiTags": {"k": "v", …},
#                "securityGroup": "…", "iamUsers": ["…", …]},
#    "host":    {"units": ["…", …], "wgInterface": "wg0", "nftTable": "ip <name>", "routeTable": 51822,
#                "paths": ["/etc/…", "/var/lib/…"]},
#    "hetzner": {"network": "…", "routes": ["10.200.0.0/15", …], "firewall": "…"}}
#
# Steps (each idempotent: what is already gone is skipped):
#
#   legacy-down.sh --config F aws [--confirm]      terminates every instance with ALL of instanceTags (pending,
#                                                  running, stopping, stopped) and waits for them; deletes the launch
#                                                  template; deregisters every AMI of this account with ALL of amiTags
#                                                  and deletes its snapshots; deletes each IAM user (its access keys,
#                                                  inline and attached policies first); deletes the security group.
#                                                  AWS credentials: the standard AWS_* environment of whoever runs it.
#   legacy-down.sh --config F host [--confirm]     on the old gateway server, as root: stops and disables the units,
#                                                  deletes the WireGuard interface, the nft table, every ip rule that
#                                                  looks up routeTable and the table's routes, then the paths. A path
#                                                  with anything mounted at or under it is refused (exit 1, nothing of
#                                                  it removed): a workspace mount left there would otherwise be
#                                                  emptied. Removal never crosses into another filesystem.
#   legacy-down.sh --config F hetzner [--confirm]  deletes the network's routes to those destinations and the firewall
#                                                  (removed from what it is applied to first). HCLOUD_TOKEN in the env.
#
# Without --confirm a step only prints what it would do ("would: …") and changes nothing. Needs bash, jq, and per
# step the aws CLI, iproute2 + nft + systemctl, or curl. MOUNTINFO overrides /proc/self/mountinfo (the offline test,
# test-legacy-down.sh).
set -euo pipefail
usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 2; }

CONFIG='' STEP='' CONFIRM=0
while [ $# -gt 0 ]; do
  case $1 in
    --config) CONFIG=${2:-}; shift 2 ;;
    --confirm) CONFIRM=1; shift ;;
    aws|host|hetzner) STEP=$1; shift ;;
    *) usage ;;
  esac
done
if [ -z "$CONFIG" ] || [ ! -s "$CONFIG" ] || [ -z "$STEP" ]; then usage; fi
cfg() { jq -er "$1" "$CONFIG"; }

# do <command…>: runs it with --confirm, else prints it
do_() {
  if [ "$CONFIRM" = 1 ]; then echo "run: $*"; "$@"; else echo "would: $*"; fi
}

step_aws() {
  local region; region=$(cfg .aws.region)
  aws() { command aws --region "$region" "$@"; }
  # instances: every tag must match
  local filters=() k v
  while IFS=$'\t' read -r k v; do filters+=("Name=tag:$k,Values=$v"); done < <(cfg '.aws.instanceTags | to_entries[] | [.key, (.value | tostring)] | @tsv')
  [ ${#filters[@]} -gt 0 ] || { echo "aws.instanceTags is empty: refusing to match every instance" >&2; exit 1; }
  local ids
  ids=$(aws ec2 describe-instances --filters "${filters[@]}" Name=instance-state-name,Values=pending,running,stopping,stopped \
    --query 'Reservations[].Instances[].InstanceId' --output text)
  if [ -n "$ids" ] && [ "$ids" != None ]; then
    # shellcheck disable=SC2086 # one id per word
    do_ aws ec2 terminate-instances --instance-ids $ids
    # shellcheck disable=SC2086
    do_ aws ec2 wait instance-terminated --instance-ids $ids
  else
    echo "ok: no instances with those tags"
  fi

  local lt; lt=$(cfg .aws.launchTemplate)
  if aws ec2 describe-launch-templates --launch-template-names "$lt" >/dev/null 2>&1; then
    do_ aws ec2 delete-launch-template --launch-template-name "$lt"
  else
    echo "ok: no launch template $lt"
  fi

  local amif=()
  while IFS=$'\t' read -r k v; do amif+=("Name=tag:$k,Values=$v"); done < <(cfg '.aws.amiTags | to_entries[] | [.key, (.value | tostring)] | @tsv')
  [ ${#amif[@]} -gt 0 ] || { echo "aws.amiTags is empty: refusing to match every image" >&2; exit 1; }
  local ami snaps
  for ami in $(aws ec2 describe-images --owners self --filters "${amif[@]}" --query 'Images[].ImageId' --output text); do
    [ "$ami" = None ] && continue
    snaps=$(aws ec2 describe-images --image-ids "$ami" --query 'Images[0].BlockDeviceMappings[].Ebs.SnapshotId' --output text)
    do_ aws ec2 deregister-image --image-id "$ami"
    for s in $snaps; do [ "$s" = None ] || do_ aws ec2 delete-snapshot --snapshot-id "$s"; done
  done

  local u x
  while read -r u; do
    if ! aws iam get-user --user-name "$u" >/dev/null 2>&1; then echo "ok: no IAM user $u"; continue; fi
    for x in $(aws iam list-access-keys --user-name "$u" --query 'AccessKeyMetadata[].AccessKeyId' --output text); do
      [ "$x" = None ] || do_ aws iam delete-access-key --user-name "$u" --access-key-id "$x"
    done
    for x in $(aws iam list-user-policies --user-name "$u" --query 'PolicyNames[]' --output text); do
      [ "$x" = None ] || do_ aws iam delete-user-policy --user-name "$u" --policy-name "$x"
    done
    for x in $(aws iam list-attached-user-policies --user-name "$u" --query 'AttachedPolicies[].PolicyArn' --output text); do
      [ "$x" = None ] || do_ aws iam detach-user-policy --user-name "$u" --policy-arn "$x"
    done
    do_ aws iam delete-user --user-name "$u"
  done < <(cfg '.aws.iamUsers[]')

  local sg sgid
  sg=$(cfg .aws.securityGroup)
  sgid=$(aws ec2 describe-security-groups --filters "Name=group-name,Values=$sg" --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
  if [ -n "$sgid" ] && [ "$sgid" != None ]; then
    do_ aws ec2 delete-security-group --group-id "$sgid"
  else
    echo "ok: no security group $sg"
  fi
}

# whether anything is mounted at or under $1 (mountinfo's mount points are field 5, octal-escaped)
mounted_under() {
  local p=${1%/}
  awk -v p="$p" '{ m = $5; gsub(/\\040/, " ", m); if (m == p || index(m, p "/") == 1) found = 1 } END { exit !found }' \
    "${MOUNTINFO:-/proc/self/mountinfo}"
}

step_host() {
  local u
  while read -r u; do
    if systemctl cat "$u" >/dev/null 2>&1; then do_ systemctl disable --now "$u"; else echo "ok: no unit $u"; fi
  done < <(cfg '.host.units[]')
  local wg; wg=$(cfg .host.wgInterface)
  if ip link show "$wg" >/dev/null 2>&1; then do_ ip link del "$wg"; else echo "ok: no interface $wg"; fi
  local nt; nt=$(cfg .host.nftTable)
  # shellcheck disable=SC2086 # "ip <name>": family and name
  if nft list table $nt >/dev/null 2>&1; then do_ nft delete table $nt; else echo "ok: no nft table $nt"; fi
  local t prio n=0; t=$(cfg .host.routeTable)
  for prio in $(ip rule show | awk -v t="$t" '$0 ~ ("lookup " t "( |$)") { sub(/:$/, "", $1); print $1 }'); do
    do_ ip rule del priority "$prio"; n=$((n + 1))
  done
  [ $n -gt 0 ] || echo "ok: no ip rule looks up table $t"
  if [ -n "$(ip route show table "$t" 2>/dev/null)" ]; then do_ ip route flush table "$t"; else echo "ok: table $t is empty"; fi
  local p refused=0
  while read -r p; do
    case $p in /|/etc|/var|/var/lib|/usr|/home|/root|"") echo "refused: $p is not a fabric path" >&2; refused=1; continue ;; esac
    [ -e "$p" ] || { echo "ok: no $p"; continue; }
    if mounted_under "$p"; then
      echo "refused: something is mounted at or under $p (unmount it first; nothing of $p removed)" >&2
      refused=1
      continue
    fi
    do_ rm -rf --one-file-system "$p"
  done < <(cfg '.host.paths[]')
  return $refused
}

step_hetzner() {
  : "${HCLOUD_TOKEN:?HCLOUD_TOKEN is not set}"
  local api=${HCLOUD_API:-https://api.hetzner.cloud/v1}
  # the token reaches curl through a config on a file descriptor, never on its command line
  hc() { curl -fsS -K <(printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN") -H 'Content-Type: application/json' "$@"; }
  local net nid body dst gw
  net=$(cfg .hetzner.network)
  body=$(hc "$api/networks?name=$net")
  nid=$(jq -r '.networks[0].id // empty' <<< "$body")
  if [ -z "$nid" ]; then
    echo "ok: no network $net"
  else
    while read -r dst; do
      gw=$(jq -r --arg d "$dst" '.networks[0].routes[] | select(.destination == $d) | .gateway' <<< "$body")
      if [ -z "$gw" ]; then echo "ok: no route $dst on $net"; continue; fi
      do_ hc -X POST "$api/networks/$nid/actions/delete_route" -d "$(jq -cn --arg d "$dst" --arg g "$gw" '{destination: $d, gateway: $g}')"
    done < <(cfg '.hetzner.routes[]')
  fi
  local fw fid applied
  fw=$(cfg .hetzner.firewall)
  body=$(hc "$api/firewalls?name=$fw")
  fid=$(jq -r '.firewalls[0].id // empty' <<< "$body")
  if [ -z "$fid" ]; then echo "ok: no firewall $fw"; return 0; fi
  applied=$(jq -c '[.firewalls[0].applied_to[] | if .type == "server" then {type, server: {id: .server.id}} else {type, label_selector} end]' <<< "$body")
  if [ "$applied" != "[]" ]; then
    do_ hc -X POST "$api/firewalls/$fid/actions/remove_from_resources" -d "{\"remove_from\": $applied}"
    # the removal is an action of its own: the firewall can be deleted once it applies to nothing (up to 2 min)
    if [ "$CONFIRM" = 1 ]; then
      local i
      for i in $(seq 60); do
        [ "$(hc "$api/firewalls/$fid" | jq '.firewall.applied_to | length')" = 0 ] && break
        [ "$i" = 60 ] && { echo "firewall $fw still applied after 2 min" >&2; exit 1; }
        sleep 2
      done
    fi
  fi
  do_ hc -X DELETE "$api/firewalls/$fid"
}

[ "$CONFIRM" = 1 ] || echo "dry run (pass --confirm to change anything)"
"step_$STEP"

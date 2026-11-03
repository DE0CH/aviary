#!/usr/bin/env bash
# Teardown of a deployment's previous sandbox fabric, once, after the switch to hermit (hermit spec 22.3 step 5; the
# switch's A6). Every name comes from the deployment's legacy file (--config, JSON; nothing here names a deployment):
#
#   {"aws":     {"region": "…", "instanceTags": {"k": "v", …}, "launchTemplate": "…", "amiTags": {"k": "v", …},
#                "keepTags": {"k": "v", …}, "securityGroup": "…", "iamUsers": ["…", …]},
#    "host":    {"hostname": "…", "units": ["…", …], "wgInterface": "wg0", "nftTable": "ip <name>",
#                "routeTable": 51822, "rules": ["iif wg0 lookup main suppress_prefixlength 16", …],
#                "paths": ["/etc/…", "/var/lib/…"]},
#    "hetzner": {"network": "…", "routes": ["10.200.0.0/15", …], "firewall": "…"}}
#
# Steps (each idempotent: what is already gone is skipped):
#
#   legacy-down.sh --config F aws [--confirm]      terminates every instance with ALL of instanceTags (pending,
#                                                  running, stopping, stopped) and waits for them; deletes the launch
#                                                  template; deregisters every AMI of this account with ALL of amiTags
#                                                  and deletes its snapshots (each tagged legacy-down-of=<ami> before
#                                                  the AMI goes, so a re-run finds a snapshot whose delete failed);
#                                                  deletes each IAM user (its access keys, login profile, MFA devices,
#                                                  signing certificates, SSH keys, service credentials, groups, inline
#                                                  and attached policies first); deletes the security group. Refused
#                                                  before any change (exit 1) when an AMI amiTags matches carries ALL
#                                                  of keepTags (the new fabric's tags, required), or is used by
#                                                  another launch template (any version, an SSM alias resolved) or by
#                                                  an instance without ALL of instanceTags (the new fabric shares the
#                                                  account). Every AWS query that fails stops the step (exit 1); only
#                                                  AWS's own "not found" means already gone. AWS credentials: the
#                                                  standard AWS_* environment.
#   legacy-down.sh --config F host [--confirm]     on the old gateway server, as root: refused (exit 1, nothing
#                                                  changed) unless `hostname` is host.hostname and no hermit gateway
#                                                  runs there (its state directory /var/lib/hermit-gateway, or a
#                                                  container of that name; a crictl on PATH that fails refuses too).
#                                                  Then stops and disables the units, deletes
#                                                  every ip rule that looks up routeTable or is one of host.rules (each
#                                                  by its full selector) and the table's routes, the WireGuard
#                                                  interface, the nft table, then the paths. A path with anything
#                                                  mounted at or under it is refused (exit 1, nothing of it removed):
#                                                  a workspace mount left there would otherwise be emptied. Removal
#                                                  never crosses into another filesystem.
#   legacy-down.sh --config F hetzner [--confirm]  deletes the network's routes to those destinations and the firewall
#                                                  (removed from what it is applied to first). HCLOUD_TOKEN in the env.
#
# Without --confirm a step only prints what it would do ("would: …") and changes nothing. Needs bash, jq, and per
# step the aws CLI, iproute2 + nft + systemctl, or curl. MOUNTINFO overrides /proc/self/mountinfo and
# HERMIT_GATEWAY_STATE /var/lib/hermit-gateway (the offline test, test-legacy-down.sh).
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
  local errf; errf=$(mktemp)
  # aq <args…>: aws's answer on stdout; any failure stops the step with aws's message. Always assigned to a variable
  # first (set -e does not see a failure inside a for list's substitution)
  aq() {
    local out
    if ! out=$(aws "$@" 2>"$errf"); then echo "aws $*: $(cat "$errf")" >&2; rm -f "$errf"; exit 1; fi
    printf '%s\n' "$out"
  }
  # afound <not-found code> <args…>: 0 when the thing exists, 1 when aws answers that code; anything else stops
  afound() {
    local code=$1; shift
    if aws "$@" >/dev/null 2>"$errf"; then return 0; fi
    if grep -q -- "$code" "$errf"; then return 1; fi
    echo "aws $*: $(cat "$errf")" >&2; rm -f "$errf"; exit 1
  }
  local filters=() k v
  while IFS=$'\t' read -r k v; do filters+=("Name=tag:$k,Values=$v"); done < <(cfg '.aws.instanceTags | to_entries[] | [.key, (.value | tostring)] | @tsv')
  [ ${#filters[@]} -gt 0 ] || { echo "aws.instanceTags is empty: refusing to match every instance" >&2; exit 1; }
  local amif=()
  while IFS=$'\t' read -r k v; do amif+=("Name=tag:$k,Values=$v"); done < <(cfg '.aws.amiTags | to_entries[] | [.key, (.value | tostring)] | @tsv')
  [ ${#amif[@]} -gt 0 ] || { echo "aws.amiTags is empty: refusing to match every image" >&2; exit 1; }
  local lt; lt=$(cfg .aws.launchTemplate)
  local keep; keep=$(jq -c '.aws.keepTags // {}' "$CONFIG")
  [ "$keep" != '{}' ] || { echo "aws.keepTags (the new fabric's tags) is empty: refusing to tell its images apart" >&2; exit 1; }

  # before any change: the AMIs to deregister must be the old fabric's alone
  local images amis lts used=' ' ltid ltname vers ami others
  images=$(aq ec2 describe-images --owners self --filters "${amif[@]}" --query 'Images[].{id: ImageId, tags: Tags}' --output json)
  amis=$(jq -r '.[]?.id' <<< "$images")
  local kept
  kept=$(jq -r --argjson keep "$keep" '.[]? | ((.tags // []) | map({key: .Key, value: .Value}) | from_entries) as $t
      | select([$keep | to_entries[] | $t[.key] == (.value | tostring)] | all) | "\(.id)(keepTags)"' <<< "$images")
  lts=$(aq ec2 describe-launch-templates --query 'LaunchTemplates[].[LaunchTemplateId,LaunchTemplateName]' --output text)
  while read -r ltid ltname; do
    if [ -z "$ltid" ] || [ "$ltid" = None ] || [ "$ltname" = "$lt" ]; then continue; fi
    vers=$(aq ec2 describe-launch-template-versions --launch-template-id "$ltid" --resolve-alias \
      --query 'LaunchTemplateVersions[].LaunchTemplateData.ImageId' --output text)
    for ami in $vers; do used="$used$ami($ltname) "; done
  done <<< "$lts"
  others=$(aq ec2 describe-instances --filters Name=instance-state-name,Values=pending,running,stopping,stopped \
    --query 'Reservations[].Instances[].{id: InstanceId, image: ImageId, tags: Tags}' --output json)
  others=$(jq -r --argjson want "$(jq -c '.aws.instanceTags' "$CONFIG")" '.[]? |
      ((.tags // []) | map({key: .Key, value: .Value}) | from_entries) as $t
      | select([$want | to_entries[] | $t[.key] == (.value | tostring)] | all | not) | "\(.image)(\(.id))"' <<< "$others")
  used="$used$(tr '\n' ' ' <<< "$others")$(tr '\n' ' ' <<< "$kept")"
  local clash=''
  for ami in $amis; do
    [ "$ami" = None ] && continue
    case $used in *" $ami("*) clash="$clash $(tr ' ' '\n' <<< "$used" | grep "^$ami(" | tr '\n' ' ')" ;; esac
  done
  if [ -n "$clash" ]; then
    echo "refused: images aws.amiTags matches are in use outside the old fabric:$clash(amiTags must name only the old fabric's images; nothing changed)" >&2
    rm -f "$errf"; exit 1
  fi

  local ids
  ids=$(aq ec2 describe-instances --filters "${filters[@]}" Name=instance-state-name,Values=pending,running,stopping,stopped \
    --query 'Reservations[].Instances[].InstanceId' --output text)
  if [ -n "$ids" ] && [ "$ids" != None ]; then
    # shellcheck disable=SC2086 # one id per word
    do_ aws ec2 terminate-instances --instance-ids $ids
    # shellcheck disable=SC2086
    do_ aws ec2 wait instance-terminated --instance-ids $ids
  else
    echo "ok: no instances with those tags"
  fi

  if afound InvalidLaunchTemplateName.NotFoundException ec2 describe-launch-templates --launch-template-names "$lt"; then
    do_ aws ec2 delete-launch-template --launch-template-name "$lt"
  else
    echo "ok: no launch template $lt"
  fi

  # each AMI's snapshots are tagged before it goes, then every tagged snapshot is deleted: a snapshot whose delete
  # failed is found again by the next run, although its AMI is gone by then
  local snaps s pending=''
  for ami in $amis; do
    [ "$ami" = None ] && continue
    snaps=$(aq ec2 describe-images --image-ids "$ami" --query 'Images[0].BlockDeviceMappings[].Ebs.SnapshotId' --output text)
    for s in $snaps; do
      [ "$s" = None ] && continue
      do_ aws ec2 create-tags --resources "$s" --tags "Key=legacy-down-of,Value=$ami"
      pending="$pending $s"
    done
    do_ aws ec2 deregister-image --image-id "$ami"
  done
  snaps=$(aq ec2 describe-snapshots --owner-ids self --filters Name=tag-key,Values=legacy-down-of \
    --query 'Snapshots[].SnapshotId' --output text)
  # shellcheck disable=SC2086 # one id per word
  for s in $(printf '%s\n' $pending $snaps | grep -v -e '^$' -e '^None$' | sort -u); do
    do_ aws ec2 delete-snapshot --snapshot-id "$s"
  done

  local u x list
  while read -r u; do
    if ! afound NoSuchEntity iam get-user --user-name "$u"; then echo "ok: no IAM user $u"; continue; fi
    list=$(aq iam list-access-keys --user-name "$u" --query 'AccessKeyMetadata[].AccessKeyId' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam delete-access-key --user-name "$u" --access-key-id "$x"; done
    list=$(aq iam list-user-policies --user-name "$u" --query 'PolicyNames[]' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam delete-user-policy --user-name "$u" --policy-name "$x"; done
    list=$(aq iam list-attached-user-policies --user-name "$u" --query 'AttachedPolicies[].PolicyArn' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam detach-user-policy --user-name "$u" --policy-arn "$x"; done
    # everything else delete-user refuses with DeleteConflict while it is there
    if afound NoSuchEntity iam get-login-profile --user-name "$u"; then do_ aws iam delete-login-profile --user-name "$u"; fi
    list=$(aq iam list-mfa-devices --user-name "$u" --query 'MFADevices[].SerialNumber' --output text)
    for x in $list; do
      [ "$x" = None ] && continue
      do_ aws iam deactivate-mfa-device --user-name "$u" --serial-number "$x"
      case $x in arn:*:mfa/*) do_ aws iam delete-virtual-mfa-device --serial-number "$x" ;; esac
    done
    list=$(aq iam list-signing-certificates --user-name "$u" --query 'Certificates[].CertificateId' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam delete-signing-certificate --user-name "$u" --certificate-id "$x"; done
    list=$(aq iam list-ssh-public-keys --user-name "$u" --query 'SSHPublicKeys[].SSHPublicKeyId' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam delete-ssh-public-key --user-name "$u" --ssh-public-key-id "$x"; done
    list=$(aq iam list-service-specific-credentials --user-name "$u" \
      --query 'ServiceSpecificCredentials[].ServiceSpecificCredentialId' --output text)
    for x in $list; do
      [ "$x" = None ] || do_ aws iam delete-service-specific-credential --user-name "$u" --service-specific-credential-id "$x"
    done
    list=$(aq iam list-groups-for-user --user-name "$u" --query 'Groups[].GroupName' --output text)
    for x in $list; do [ "$x" = None ] || do_ aws iam remove-user-from-group --user-name "$u" --group-name "$x"; done
    do_ aws iam delete-user --user-name "$u"
  done < <(cfg '.aws.iamUsers[]')

  local sg sgid
  sg=$(cfg .aws.securityGroup)
  sgid=$(aq ec2 describe-security-groups --filters "Name=group-name,Values=$sg" --query 'SecurityGroups[0].GroupId' --output text)
  if [ -n "$sgid" ] && [ "$sgid" != None ]; then
    do_ aws ec2 delete-security-group --group-id "$sgid"
  else
    echo "ok: no security group $sg"
  fi
  rm -f "$errf"
}

# whether anything is mounted at or under $1 (mountinfo's mount points are field 5, octal-escaped)
mounted_under() {
  local p=${1%/}
  awk -v p="$p" '{ m = $5; gsub(/\\040/, " ", m); if (m == p || index(m, p "/") == 1) found = 1 } END { exit !found }' \
    "${MOUNTINFO:-/proc/self/mountinfo}"
}

step_host() {
  # the right server: its name, and no hermit gateway (which uses the same interface, table and nft names)
  local want have; want=$(cfg .host.hostname); have=$(hostname)
  if [ "$have" != "$want" ]; then
    echo "refused: this is $have, not the old gateway $want (nothing changed)" >&2; exit 1
  fi
  local hg=${HERMIT_GATEWAY_STATE:-/var/lib/hermit-gateway} running=''
  if command -v crictl >/dev/null 2>&1; then
    # a crictl that cannot answer (no runtime endpoint, no permission) cannot show there is no gateway
    local cerr; cerr=$(mktemp)
    if ! running=$(crictl ps -q --name hermit-gateway 2>"$cerr"); then
      echo "refused: crictl cannot list the containers on $have ($(cat "$cerr")), so a hermit gateway cannot be ruled out (nothing changed)" >&2
      rm -f "$cerr"; exit 1
    fi
    rm -f "$cerr"
  fi
  if [ -e "$hg" ] || [ -n "$running" ]; then
    echo "refused: a hermit gateway runs on $have ($hg or its container): its wg0, table and nft table would go (nothing changed)" >&2
    exit 1
  fi
  local u
  while read -r u; do
    if systemctl cat "$u" >/dev/null 2>&1; then do_ systemctl disable --now "$u"; else echo "ok: no unit $u"; fi
  done < <(cfg '.host.units[]')
  # rules first (a rule outlives its interface, shown "[detached]"), each by its full selector: `ip rule del priority
  # N` alone removes the first rule at N, whatever it is
  local t n=0 line prio sel extra; t=$(cfg .host.routeTable)
  extra=$(jq -r '.host.rules // [] | .[]' "$CONFIG")
  local shown; shown=$(ip rule show)
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    prio=${line%%:*}
    sel=$(sed -e 's/^[0-9]*:[[:space:]]*//' -e 's/ \[detached\]//g' -e 's/[[:space:]]*$//' <<< "$line")
    if [[ " $sel " == *" lookup $t "* ]] || { [ -n "$extra" ] && grep -qxF -- "${sel#from all }" <<< "$extra"; }; then
      # shellcheck disable=SC2086 # the selector's words
      do_ ip rule del priority "$prio" $sel; n=$((n + 1))
    fi
  done <<< "$shown"
  [ $n -gt 0 ] || echo "ok: no ip rule looks up table $t or is one of host.rules"
  if [ -n "$(ip route show table "$t" 2>/dev/null)" ]; then do_ ip route flush table "$t"; else echo "ok: table $t is empty"; fi
  local wg; wg=$(cfg .host.wgInterface)
  if ip link show "$wg" >/dev/null 2>&1; then do_ ip link del "$wg"; else echo "ok: no interface $wg"; fi
  local nt; nt=$(cfg .host.nftTable)
  # shellcheck disable=SC2086 # "ip <name>": family and name
  if nft list table $nt >/dev/null 2>&1; then do_ nft delete table $nt; else echo "ok: no nft table $nt"; fi
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

#!/usr/bin/env bash
# shellcheck disable=SC2016 # JMESPath queries: their backticks and $Default are the AWS CLI's, not the shell's
# AWS side of hermit's fabric (crates/sandboxd/docs/HOST.md, "Burst hosts"). Every deployment value comes from the
# deployment's hermit.json (hermit spec 9), given with --config: region, subnet, launch template name (also the
# security group's name), tags, and the three pins sandbox.image, fabric.sandboxd, fabric.runsc. Nothing here names a
# deployment. AWS credentials: the standard AWS_* environment (or profile) of whoever runs it; the region is the
# config's. Idempotent steps:
#
#   infra.sh --config F sg             security group <template name>: WireGuard (udp 51820) from the VPC only (hosts
#                                      peer each other for migrations; each dials the gateway itself); nothing else in
#   infra.sh --config F ipv6           IPv6 for the hosts (free): an Amazon /56 on the VPC, a /64 on the subnet, ::/0
#                                      to the internet gateway, IPv6 egress in the SG. The gateway (a cluster node,
#                                      IPv6 only from the outside) is reached over it.
#   infra.sh --config F ami [image]    bakes an AMI: Ubuntu 24.04 + host/setup.sh burst (the gVisor fork release
#                                      fabric.runsc, sandboxd from release fabric.sandboxd, the sandbox image unpacked);
#                                      a temporary t3.large builder + key + SSH rule, removed after. Image: sandbox.image
#                                      unless given. A private image needs GHCR_USER/GHCR_TOKEN (read:packages). Tags
#                                      the AMI with what it holds: image=<manifest digest>, sandboxd=<release>,
#                                      runsc=<release>, plus the config's tags.
#   infra.sh --config F current        the launch template's default AMI and its tags (image, sandboxd, runsc)
#   infra.sh --config F check          exit 0 when that AMI holds exactly the three pins, else 1
#   infra.sh --config F sync           makes the template's AMI hold the pins: if `check` fails (or FORCE=1), bakes an
#                                      AMI from the pins, then `template` and `prune-amis` (the deployment's AMI
#                                      workflow runs it when a pin changes, hermit spec 14.3)
#   infra.sh --config F template <ami> launch template (new default version): fabric.aws.instanceType (default m7i.xlarge), that AMI, the SG, a public
#                                      IPv4 and an IPv6 address, one subnet (one AZ: host-to-host traffic is free),
#                                      IMDSv2 with hop limit 1, no instance profile, no key pair, gp3 40 GB deleted with
#                                      the instance, terminate on shutdown; the template's other versions are deleted
#   infra.sh --config F iam            IAM user hermit-scaler with only: RunInstances from that template (its instance type,
#                                      tag app=<tags.app>), tags at launch, TerminateInstances of app=<tags.app>
#                                      instances, DescribeInstances (scaler-policy.json). Prints nothing secret;
#                                      `iam-key` makes an access key and writes it to $KEY_OUT (mode 600), never stdout.
#   infra.sh --config F ci-iam         IAM user hermit-ami-ci for the AMI workflow (ami-ci-policy.json: a tagged
#                                      t3.large builder, its key pair and SSH rule, the image, this template, pruning
#                                      tagged AMIs); `ci-iam-key` writes a new access key to $KEY_OUT (mode 600)
#   infra.sh --config F prune-amis     deregisters every AMI tagged app=<tags.app> but the template's (and snapshots)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
usage() { sed -n '3,/^set -euo/p' "$0" | head -n -1; exit 2; }

CONFIG=
if [ "${1:-}" = --config ]; then CONFIG=${2:?--config hermit.json}; shift 2; fi
if [ -z "$CONFIG" ] || [ ! -s "$CONFIG" ]; then usage; fi
cfg() { jq -er "$1" "$CONFIG"; }
REGION=$(cfg .fabric.aws.region)
SUBNET=$(cfg .fabric.aws.subnet)
NAME=$(cfg .fabric.aws.launchTemplate)
APP=$(cfg .fabric.aws.tags.app)
TAGS_JSON=$(cfg '.fabric.aws.tags | to_entries | map({Key: .key, Value: (.value | tostring)})')
PIN_IMAGE=$(cfg .sandbox.image)
PIN_SANDBOXD=$(cfg .fabric.sandboxd)
PIN_RUNSC=$(cfg .fabric.runsc)
INSTANCE_TYPE=$(jq -r '.fabric.aws.instanceType // "m7i.xlarge"' "$CONFIG")   # hermit spec 13.1's host size
SCALER_USER=hermit-scaler
CI_USER=hermit-ami-ci

aws() { command aws --region "$REGION" "$@"; }
# the config's tags plus extra Key=Value pairs, as an EC2 TagSpecification for one resource type
tagspec() {
  local type=$1; shift
  local extra='[]' kv
  for kv in "$@"; do extra=$(jq -c --arg k "${kv%%=*}" --arg v "${kv#*=}" '. + [{Key: $k, Value: $v}]' <<< "$extra"); done
  jq -c --arg t "$type" --argjson extra "$extra" '{ResourceType: $t, Tags: ((. + $extra) | reverse | unique_by(.Key))}' <<< "$TAGS_JSON"
}

# the default template version's AMI, and one of its tags
cur_ami() { aws ec2 describe-launch-template-versions --launch-template-name "$NAME" --versions '$Default' --query 'LaunchTemplateVersions[0].LaunchTemplateData.ImageId' --output text; }
ami_tag() { aws ec2 describe-images --image-ids "$1" --query "Images[0].Tags[?Key=='$2'].Value | [0]" --output text | grep -v '^None$' || true; }
# a registry login for crane, only when GHCR_USER/GHCR_TOKEN are set (a public image needs none)
with_login() {
  if [ -n "${GHCR_USER:-}" ] && [ -n "${GHCR_TOKEN:-}" ]; then
    local d rc=0; d=$(mktemp -d)
    printf '{"auths":{"ghcr.io":{"auth":"%s"}}}' "$(printf '%s:%s' "$GHCR_USER" "$GHCR_TOKEN" | base64 -w0)" > "$d/config.json"
    DOCKER_CONFIG=$d "$@" || rc=$?; rm -rf "$d"; return $rc
  fi
  "$@"
}
# <image ref>'s manifest digest (64 hex): from the ref itself when pinned by digest
digest_of() {
  case $1 in
    *@sha256:*) echo "${1##*@sha256:}" ;;
    *) with_login crane digest "$1" | sed 's/^sha256://' ;;
  esac
}

sg_id() { aws ec2 describe-security-groups --filters Name=group-name,Values="$NAME" --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null | grep -v None || true; }
# policy FILE ACCOUNT LT_ID: the policy with its placeholders filled in, in one pass (a value is never read as another
# placeholder). Every value must be a plain name ([A-Za-z0-9._-]), so nothing in the config can change the policy's
# shape or widen an ARN.
policy() {
  jq --arg ACCOUNT "$2" --arg LT_ID "$3" --arg REGION "$REGION" --arg TAG_APP "$APP" --arg TEMPLATE "$NAME" \
     --arg INSTANCE_TYPE "$INSTANCE_TYPE" '
    $ARGS.named as $v
    | [$v | to_entries[] | select(.value | test("^[A-Za-z0-9][A-Za-z0-9._-]*$") | not) | .key] as $bad
    | if ($bad | length) > 0 then error("not a plain name in the config: \($bad | join(", "))") else . end
    | walk(if type == "string" then gsub("(?<p>ACCOUNT|LT_ID|REGION|TAG_APP|TEMPLATE|INSTANCE_TYPE)"; $v[.p]) else . end)
  ' "$here/$1"
}

case ${1:-} in
sg)
  id=$(sg_id)
  if [ -z "$id" ]; then
    vpc=$(aws ec2 describe-subnets --subnet-ids "$SUBNET" --query 'Subnets[0].VpcId' --output text)
    vpc_cidr=$(aws ec2 describe-vpcs --vpc-ids "$vpc" --query 'Vpcs[0].CidrBlock' --output text)
    id=$(aws ec2 create-security-group --group-name "$NAME" --description "hermit sandbox hosts: WireGuard between hosts" --vpc-id "$vpc" \
      --tag-specifications "$(tagspec security-group)" --query GroupId --output text)
    aws ec2 authorize-security-group-ingress --group-id "$id" --ip-permissions "IpProtocol=udp,FromPort=51820,ToPort=51820,IpRanges=[{CidrIp=$vpc_cidr,Description=sandbox hosts}]" >/dev/null
  fi
  echo "$id"
  ;;
ipv6)
  vpc=$(aws ec2 describe-subnets --subnet-ids "$SUBNET" --query 'Subnets[0].VpcId' --output text)
  v6=$(aws ec2 describe-vpcs --vpc-ids "$vpc" --query 'Vpcs[0].Ipv6CidrBlockAssociationSet[?Ipv6CidrBlockState.State==`associated`].Ipv6CidrBlock | [0]' --output text)
  if [ -z "$v6" ] || [ "$v6" = None ]; then
    aws ec2 associate-vpc-cidr-block --vpc-id "$vpc" --amazon-provided-ipv6-cidr-block >/dev/null
    for _ in $(seq 30); do
      v6=$(aws ec2 describe-vpcs --vpc-ids "$vpc" --query 'Vpcs[0].Ipv6CidrBlockAssociationSet[?Ipv6CidrBlockState.State==`associated`].Ipv6CidrBlock | [0]' --output text)
      [ -n "$v6" ] && [ "$v6" != None ] && break; sleep 2
    done
  fi
  sub6=$(aws ec2 describe-subnets --subnet-ids "$SUBNET" --query 'Subnets[0].Ipv6CidrBlockAssociationSet[0].Ipv6CidrBlock' --output text)
  if [ -z "$sub6" ] || [ "$sub6" = None ]; then
    # the subnet's /64: the /56's first one (one subnet uses IPv6 here)
    sub6="${v6%::/56}::/64"
    aws ec2 associate-subnet-cidr-block --subnet-id "$SUBNET" --ipv6-cidr-block "$sub6" >/dev/null
  fi
  rt=$(aws ec2 describe-route-tables --filters Name=association.subnet-id,Values="$SUBNET" --query 'RouteTables[0].RouteTableId' --output text)
  [ -n "$rt" ] && [ "$rt" != None ] || rt=$(aws ec2 describe-route-tables --filters Name=vpc-id,Values="$vpc" Name=association.main,Values=true --query 'RouteTables[0].RouteTableId' --output text)
  igw=$(aws ec2 describe-internet-gateways --filters Name=attachment.vpc-id,Values="$vpc" --query 'InternetGateways[0].InternetGatewayId' --output text)
  aws ec2 create-route --route-table-id "$rt" --destination-ipv6-cidr-block ::/0 --gateway-id "$igw" >/dev/null 2>&1 || true
  sg=$(sg_id)
  [ -z "$sg" ] || aws ec2 authorize-security-group-egress --group-id "$sg" --ip-permissions 'IpProtocol=-1,Ipv6Ranges=[{CidrIpv6=::/0,Description=gateway over IPv6}]' >/dev/null 2>&1 || true
  echo "vpc $vpc $v6, subnet $SUBNET $sub6, route ::/0 -> $igw in $rt, sg $sg"
  ;;
ami)
  IMAGE=${2:-$PIN_IMAGE}
  rel=${SANDBOXD_RELEASE:-$PIN_SANDBOXD}
  gv=${GV:-$PIN_RUNSC}
  sg=$(sg_id); [ -n "$sg" ] || { echo "run infra.sh sg first" >&2; exit 1; }
  t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
  ssh-keygen -q -t ed25519 -N "" -f "$t/key"
  kp=$NAME-ami-$(date +%s)
  aws ec2 import-key-pair --key-name "$kp" --public-key-material "fileb://$t/key.pub" >/dev/null
  myip=$(curl -fsS https://checkip.amazonaws.com)
  aws ec2 authorize-security-group-ingress --group-id "$sg" --ip-permissions "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=$myip/32,Description=ami build}]" >/dev/null
  base=$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)
  iid=$(aws ec2 run-instances --image-id "$base" --instance-type t3.large --key-name "$kp" --subnet-id "$SUBNET" --security-group-ids "$sg" \
    --associate-public-ip-address --metadata-options HttpTokens=required,HttpPutResponseHopLimit=1 \
    --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=16,VolumeType=gp3,DeleteOnTermination=true}' \
    --tag-specifications "$(tagspec instance "app=$APP-build" "Name=$NAME-ami-build")" \
    --query 'Instances[0].InstanceId' --output text)
  cleanup() {
    aws ec2 terminate-instances --instance-ids "$iid" >/dev/null || true
    aws ec2 revoke-security-group-ingress --group-id "$sg" --ip-permissions "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=$myip/32}]" >/dev/null 2>&1 || true
    aws ec2 delete-key-pair --key-name "$kp" >/dev/null || true
    rm -rf "$t"
  }
  trap cleanup EXIT
  echo "builder $iid ($(date -u +%H:%M:%SZ))" >&2
  aws ec2 wait instance-running --instance-ids "$iid"
  ip=$(aws ec2 describe-instances --instance-ids "$iid" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
  o=(-i "$t/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5)
  until ssh "${o[@]}" "ubuntu@$ip" true 2>/dev/null; do sleep 3; done
  ssh "${o[@]}" "ubuntu@$ip" "cloud-init status --wait >/dev/null 2>&1 || true; mkdir -p sbx"
  # sandboxd from the pinned aviary release (SANDBOXD_BINARY for a local build), checked against its SHA256SUMS
  bin=$(mktemp -d); bash "$here/../host/fetch-sandboxd.sh" "$bin" "$rel" >&2
  rel=$(cat "$bin/bin/RELEASE")
  tar -cf - -C "$here/.." host -C "$bin" bin | ssh "${o[@]}" "ubuntu@$ip" "tar -xf - -C sbx"; rm -rf "$bin"
  auth=
  if [ -n "${GHCR_USER:-}" ] && [ -n "${GHCR_TOKEN:-}" ]; then
    # the registry login exists only for the pull, in a file the setup deletes; it never reaches the AMI
    printf '{"auths":{"ghcr.io":{"auth":"%s"}}}' "$(printf '%s:%s' "$GHCR_USER" "$GHCR_TOKEN" | base64 -w0)" | ssh "${o[@]}" "ubuntu@$ip" "umask 077; cat > /tmp/ghcr.json"
    auth="REGISTRY_AUTH_FILE=/tmp/ghcr.json"
  fi
  # shellcheck disable=SC2029 # expanded here on purpose: the builder gets the values
  ssh "${o[@]}" "ubuntu@$ip" "sudo GV=$gv $auth bash sbx/host/setup.sh burst $IMAGE; rm -f /tmp/ghcr.json" >&2
  digest=$(ssh "${o[@]}" "ubuntu@$ip" "sudo ls /var/lib/sbx/images | grep -E '^[0-9a-f]{64}$' | head -1")
  [ -n "$digest" ] || { echo "no image on the builder" >&2; exit 1; }
  # no way in afterwards: the build key goes, and instances from this AMI get no key pair
  ssh "${o[@]}" "ubuntu@$ip" "sudo rm -rf /home/ubuntu/sbx /home/ubuntu/.ssh/authorized_keys /root/.ssh/authorized_keys; sudo sync" >&2
  aws ec2 stop-instances --instance-ids "$iid" >/dev/null
  aws ec2 wait instance-stopped --instance-ids "$iid"
  ami=$(aws ec2 create-image --instance-id "$iid" --name "$NAME-$(date -u +%Y%m%d-%H%M)" \
    --description "hermit sandbox host: gVisor fork $gv, sandboxd $rel, sandbox image ${digest:0:12}" \
    --tag-specifications "$(tagspec image "image=$digest" "sandboxd=$rel" "runsc=$gv")" "$(tagspec snapshot)" \
    --query ImageId --output text)
  aws ec2 wait image-available --image-ids "$ami"
  echo "$ami"
  ;;
template)
  AMI=${2:?ami id}
  sg=$(sg_id)
  data=$(jq -cn --arg ami "$AMI" --arg sg "$sg" --arg subnet "$SUBNET" --arg type "$INSTANCE_TYPE" --argjson eni "$(tagspec network-interface)" '{
    ImageId: $ami, InstanceType: $type,
    NetworkInterfaces: [{DeviceIndex: 0, SubnetId: $subnet, Groups: [$sg], AssociatePublicIpAddress: true, Ipv6AddressCount: 1, DeleteOnTermination: true}],
    MetadataOptions: {HttpTokens: "required", HttpPutResponseHopLimit: 1, HttpEndpoint: "enabled", InstanceMetadataTags: "disabled"},
    BlockDeviceMappings: [{DeviceName: "/dev/sda1", Ebs: {VolumeSize: 40, VolumeType: "gp3", DeleteOnTermination: true}}],
    InstanceInitiatedShutdownBehavior: "terminate",
    TagSpecifications: [$eni]
  }')
  if aws ec2 describe-launch-templates --launch-template-names "$NAME" >/dev/null 2>&1; then
    v=$(aws ec2 create-launch-template-version --launch-template-name "$NAME" --launch-template-data "$data" --query LaunchTemplateVersion.VersionNumber --output text)
    aws ec2 modify-launch-template --launch-template-name "$NAME" --default-version "$v" >/dev/null
    old=$(aws ec2 describe-launch-template-versions --launch-template-name "$NAME" --query "LaunchTemplateVersions[?VersionNumber!=\`$v\`].VersionNumber" --output text)
    # shellcheck disable=SC2086 # a list of version numbers
    [ -z "$old" ] || aws ec2 delete-launch-template-versions --launch-template-name "$NAME" --versions $old >/dev/null
  else
    aws ec2 create-launch-template --launch-template-name "$NAME" --launch-template-data "$data" \
      --tag-specifications "$(tagspec launch-template)" >/dev/null
  fi
  aws ec2 describe-launch-templates --launch-template-names "$NAME" --query 'LaunchTemplates[0].[LaunchTemplateId,DefaultVersionNumber]' --output text
  ;;
iam)
  acct=$(aws sts get-caller-identity --query Account --output text)
  lt=$(aws ec2 describe-launch-templates --launch-template-names "$NAME" --query 'LaunchTemplates[0].LaunchTemplateId' --output text)
  pol=$(policy scaler-policy.json "$acct" "$lt" | jq -c .)
  aws iam get-user --user-name "$SCALER_USER" >/dev/null 2>&1 || aws iam create-user --user-name "$SCALER_USER" --tags "Key=app,Value=$APP" >/dev/null
  aws iam put-user-policy --user-name "$SCALER_USER" --policy-name hermit-hosts --policy-document "$pol"
  echo "user $SCALER_USER: inline policy hermit-hosts (template $lt)"
  ;;
ci-iam)
  acct=$(aws sts get-caller-identity --query Account --output text)
  lt=$(aws ec2 describe-launch-templates --launch-template-names "$NAME" --query 'LaunchTemplates[0].LaunchTemplateId' --output text)
  pol=$(policy ami-ci-policy.json "$acct" "$lt" | jq -c .)   # inline user policies: 2048 characters at most
  aws iam get-user --user-name "$CI_USER" >/dev/null 2>&1 || aws iam create-user --user-name "$CI_USER" --tags "Key=app,Value=$APP" >/dev/null
  aws iam put-user-policy --user-name "$CI_USER" --policy-name hermit-ami --policy-document "$pol"
  echo "user $CI_USER: inline policy hermit-ami (template $lt)"
  ;;
ci-iam-key|iam-key)
  : "${KEY_OUT:?file to write the new access key to}"
  user=$SCALER_USER; [ "$1" = ci-iam-key ] && user=$CI_USER
  (umask 077; aws iam create-access-key --user-name "$user" --query 'AccessKey.[AccessKeyId,SecretAccessKey]' --output text > "$KEY_OUT")
  echo "access key of $user written to $KEY_OUT"
  ;;
prune-amis)
  keep=$(cur_ami)
  aws ec2 describe-images --owners self --filters "Name=tag:app,Values=$APP" --query 'Images[].[ImageId,BlockDeviceMappings[0].Ebs.SnapshotId]' --output text |
    while read -r ami snap; do
      [ "$ami" = "$keep" ] && continue
      aws ec2 deregister-image --image-id "$ami" >/dev/null; aws ec2 delete-snapshot --snapshot-id "$snap"; echo "deleted $ami $snap"
    done
  ;;
current)
  ami=$(cur_ami)
  echo "$ami image=$(ami_tag "$ami" image) sandboxd=$(ami_tag "$ami" sandboxd) runsc=$(ami_tag "$ami" runsc)"
  ;;
check)
  want=$(digest_of "$PIN_IMAGE"); ami=$(cur_ami)
  have=$(ami_tag "$ami" image); have_rel=$(ami_tag "$ami" sandboxd); have_gv=$(ami_tag "$ami" runsc)
  if [ -n "$want" ] && [ "$want" = "$have" ] && [ "$PIN_SANDBOXD" = "$have_rel" ] && [ "$PIN_RUNSC" = "$have_gv" ]; then
    echo "ok: $ami holds image $want, sandboxd $have_rel, runsc $have_gv"; exit 0
  fi
  echo "stale: $ami holds image ${have:-?} sandboxd ${have_rel:-?} runsc ${have_gv:-?}; the pins are $want $PIN_SANDBOXD $PIN_RUNSC" >&2
  exit 1
  ;;
sync)
  if [ -z "${FORCE:-}" ] && bash "$here/infra.sh" --config "$CONFIG" check; then exit 0; fi
  new=$(bash "$here/infra.sh" --config "$CONFIG" ami | tail -1)
  [[ $new == ami-* ]] || { echo "bake failed" >&2; exit 1; }
  bash "$here/infra.sh" --config "$CONFIG" template "$new"
  bash "$here/infra.sh" --config "$CONFIG" prune-amis
  bash "$here/infra.sh" --config "$CONFIG" current
  ;;
*) usage ;;
esac

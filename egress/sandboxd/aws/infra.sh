#!/usr/bin/env bash
# AWS side of sandbox provider "vm" (docs/SANDBOX-VM.md, "Burst hosts"), in YYC's account (keys YYC_AWS_*, never the
# plain AWS_*; ../proto/aws.sh), region eu-central-1. Idempotent steps, run by hand with the account's admin keys:
#
#   infra.sh sg                    security group yyc-ai-sandbox-host: WireGuard (udp 51820) from the VPC only (hosts
#                                  peer each other for migrations; each dials the hub itself); no SSH, nothing else in
#   infra.sh ami <image ref>       bakes an AMI: Ubuntu 24.04 + host/setup.sh burst (Node, the gVisor fork, sandboxd,
#                                  the sandbox image unpacked); a temporary t3.large builder + key + SSH rule, removed
#                                  after. Needs GHCR_USER/GHCR_TOKEN (read:packages) for the private image.
#   infra.sh template <ami id>     launch template yyc-ai-sandbox-host (new default version): m7i.xlarge, that AMI, the
#                                  SG, one subnet (one AZ: host-to-host traffic is free), IMDSv2 with hop limit 1, no
#                                  instance profile, no key pair, gp3 40 GB deleted with the instance, terminate on
#                                  shutdown
#   infra.sh iam                   IAM user yyc-ai-sandbox-scaler with only: RunInstances from that template (m7i.xlarge,
#                                  tag app=yyc-ai-sandbox), tags at launch, TerminateInstances of app=yyc-ai-sandbox
#                                  instances, DescribeInstances. Prints nothing secret; `infra.sh iam-key` makes an
#                                  access key and writes it to a file given as $KEY_OUT (mode 600), never to stdout.
#   infra.sh prune-amis            deregisters all but the newest 2 sandbox AMIs (and their snapshots)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
aws() { "$here/../proto/aws.sh" "$@"; }
NAME=yyc-ai-sandbox-host
SUBNET=${SBX_SUBNET:-subnet-009eab323e09d6338}   # eu-central-1a, default VPC
VPC_CIDR=172.31.0.0/16

sg_id() { aws ec2 describe-security-groups --filters Name=group-name,Values=$NAME --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null | grep -v None || true; }

case ${1:-} in
sg)
  id=$(sg_id)
  if [ -z "$id" ]; then
    vpc=$(aws ec2 describe-subnets --subnet-ids "$SUBNET" --query 'Subnets[0].VpcId' --output text)
    id=$(aws ec2 create-security-group --group-name $NAME --description "YYC AI sandbox hosts: WireGuard between hosts" --vpc-id "$vpc" \
      --tag-specifications 'ResourceType=security-group,Tags=[{Key=app,Value=yyc-ai-sandbox}]' --query GroupId --output text)
    aws ec2 authorize-security-group-ingress --group-id "$id" --ip-permissions "IpProtocol=udp,FromPort=51820,ToPort=51820,IpRanges=[{CidrIp=$VPC_CIDR,Description=sandbox hosts}]" >/dev/null
  fi
  echo "$id"
  ;;
ami)
  IMAGE=${2:?image ref}
  : "${GHCR_USER:?}" "${GHCR_TOKEN:?}"
  sg=$(sg_id); [ -n "$sg" ] || { echo "run infra.sh sg first" >&2; exit 1; }
  t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
  ssh-keygen -q -t ed25519 -N "" -f "$t/key"
  kp=yyc-ai-sandbox-ami-$(date +%s)
  aws ec2 import-key-pair --key-name "$kp" --public-key-material "fileb://$t/key.pub" >/dev/null
  myip=$(curl -fsS https://checkip.amazonaws.com)
  aws ec2 authorize-security-group-ingress --group-id "$sg" --ip-permissions "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=$myip/32,Description=ami build}]" >/dev/null
  base=$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)
  iid=$(aws ec2 run-instances --image-id "$base" --instance-type t3.large --key-name "$kp" --subnet-id "$SUBNET" --security-group-ids "$sg" \
    --associate-public-ip-address --metadata-options HttpTokens=required,HttpPutResponseHopLimit=1 \
    --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=16,VolumeType=gp3,DeleteOnTermination=true}' \
    --tag-specifications 'ResourceType=instance,Tags=[{Key=app,Value=yyc-ai-sandbox-build},{Key=Name,Value=yyc-ai-sandbox-ami-build}]' \
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
  tar -C "$here/.." -cf - host src | ssh "${o[@]}" "ubuntu@$ip" "tar -xf - -C sbx"
  # the registry login exists only for the pull, in a file the setup deletes; it never reaches the AMI
  printf '{"auths":{"ghcr.io":{"auth":"%s"}}}' "$(printf '%s:%s' "$GHCR_USER" "$GHCR_TOKEN" | base64 -w0)" | ssh "${o[@]}" "ubuntu@$ip" "umask 077; cat > /tmp/ghcr.json"
  ssh "${o[@]}" "ubuntu@$ip" "sudo REGISTRY_AUTH_FILE=/tmp/ghcr.json bash sbx/host/setup.sh burst $IMAGE; rm -f /tmp/ghcr.json" >&2
  digest=$(ssh "${o[@]}" "ubuntu@$ip" "sudo ls /var/lib/sbx/images | grep -E '^[0-9a-f]{64}$' | head -1")
  [ -n "$digest" ] || { echo "no image on the builder" >&2; exit 1; }
  # no way in afterwards: the build key goes, and instances from this AMI get no key pair
  ssh "${o[@]}" "ubuntu@$ip" "sudo rm -rf /home/ubuntu/sbx /home/ubuntu/.ssh/authorized_keys /root/.ssh/authorized_keys; sudo sync" >&2
  aws ec2 stop-instances --instance-ids "$iid" >/dev/null
  aws ec2 wait instance-stopped --instance-ids "$iid"
  ami=$(aws ec2 create-image --instance-id "$iid" --name "yyc-ai-sandbox-host-$(date -u +%Y%m%d-%H%M)" \
    --description "YYC AI sandbox burst host: gVisor fork, sandboxd, sandbox image ${digest:0:12}" \
    --tag-specifications "ResourceType=image,Tags=[{Key=app,Value=yyc-ai-sandbox},{Key=image,Value=${digest}}]" "ResourceType=snapshot,Tags=[{Key=app,Value=yyc-ai-sandbox}]" \
    --query ImageId --output text)
  aws ec2 wait image-available --image-ids "$ami"
  echo "$ami"
  ;;
template)
  AMI=${2:?ami id}
  sg=$(sg_id)
  data=$(jq -cn --arg ami "$AMI" --arg sg "$sg" --arg subnet "$SUBNET" '{
    ImageId: $ami, InstanceType: "m7i.xlarge",
    NetworkInterfaces: [{DeviceIndex: 0, SubnetId: $subnet, Groups: [$sg], AssociatePublicIpAddress: true, DeleteOnTermination: true}],
    MetadataOptions: {HttpTokens: "required", HttpPutResponseHopLimit: 1, HttpEndpoint: "enabled", InstanceMetadataTags: "disabled"},
    BlockDeviceMappings: [{DeviceName: "/dev/sda1", Ebs: {VolumeSize: 40, VolumeType: "gp3", DeleteOnTermination: true}}],
    InstanceInitiatedShutdownBehavior: "terminate",
    TagSpecifications: [{ResourceType: "network-interface", Tags: [{Key: "app", Value: "yyc-ai-sandbox"}]}]
  }')
  if aws ec2 describe-launch-templates --launch-template-names $NAME >/dev/null 2>&1; then
    v=$(aws ec2 create-launch-template-version --launch-template-name $NAME --launch-template-data "$data" --query LaunchTemplateVersion.VersionNumber --output text)
    aws ec2 modify-launch-template --launch-template-name $NAME --default-version "$v" >/dev/null
  else
    aws ec2 create-launch-template --launch-template-name $NAME --launch-template-data "$data" \
      --tag-specifications 'ResourceType=launch-template,Tags=[{Key=app,Value=yyc-ai-sandbox}]' >/dev/null
  fi
  aws ec2 describe-launch-templates --launch-template-names $NAME --query 'LaunchTemplates[0].[LaunchTemplateId,DefaultVersionNumber]' --output text
  ;;
iam)
  acct=$(aws sts get-caller-identity --query Account --output text)
  lt=$(aws ec2 describe-launch-templates --launch-template-names $NAME --query 'LaunchTemplates[0].LaunchTemplateId' --output text)
  pol=$(sed -e "s/ACCOUNT/$acct/g" -e "s/LT_ID/$lt/g" "$here/scaler-policy.json")
  aws iam get-user --user-name yyc-ai-sandbox-scaler >/dev/null 2>&1 || aws iam create-user --user-name yyc-ai-sandbox-scaler --tags Key=app,Value=yyc-ai-sandbox >/dev/null
  aws iam put-user-policy --user-name yyc-ai-sandbox-scaler --policy-name sandbox-hosts --policy-document "$pol"
  echo "user yyc-ai-sandbox-scaler: inline policy sandbox-hosts (template $lt)"
  ;;
iam-key)
  : "${KEY_OUT:?file to write the new access key to}"
  (umask 077; aws iam create-access-key --user-name yyc-ai-sandbox-scaler --query 'AccessKey.[AccessKeyId,SecretAccessKey]' --output text > "$KEY_OUT")
  echo "access key written to $KEY_OUT"
  ;;
prune-amis)
  aws ec2 describe-images --owners self --filters Name=tag:app,Values=yyc-ai-sandbox --query 'sort_by(Images,&CreationDate)[].[ImageId,BlockDeviceMappings[0].Ebs.SnapshotId]' --output text | head -n -2 |
    while read -r ami snap; do aws ec2 deregister-image --image-id "$ami"; aws ec2 delete-snapshot --snapshot-id "$snap"; echo "deleted $ami $snap"; done
  ;;
*) sed -n '2,25p' "$0"; exit 2 ;;
esac

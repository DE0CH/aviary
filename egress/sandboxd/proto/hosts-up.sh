#!/usr/bin/env bash
# Prototype hosts for the gVisor live-migration test (docs/SANDBOX-VM.md, "Prototype"): two ordinary EC2 VMs
# (hosts A and B, no KVM, gVisor systrap) and one small VM standing in for the egress proxy and the controller.
# Everything is tagged app=yyc-ai-sandbox; hosts-down.sh terminates it all. State: $STATE (default ./state).
# Usage: hosts-up.sh [host type, default m7i.large] [egress type, default t3.small]
# NESTED=1: hosts get nested virtualization (/dev/kvm inside; m7i/c7i/r7i/m8i/... only), for the microVM variant.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
aws="$here/aws.sh"
STATE=${STATE:-$here/state}; mkdir -p "$STATE"
HOST_TYPE=${1:-m7i.large}; EGRESS_TYPE=${2:-t3.small}
TAG='ResourceType=instance,Tags=[{Key=app,Value=yyc-ai-sandbox},{Key=Name,Value=yyc-sbx-NAME}]'

[ -f "$STATE/key" ] || ssh-keygen -q -t ed25519 -N '' -f "$STATE/key" -C yyc-sbx-proto
"$aws" ec2 describe-key-pairs --key-names yyc-sbx-proto >/dev/null 2>&1 ||
  "$aws" ec2 import-key-pair --key-name yyc-sbx-proto --public-key-material "fileb://$STATE/key.pub" \
    --tag-specifications 'ResourceType=key-pair,Tags=[{Key=app,Value=yyc-ai-sandbox}]' >/dev/null

vpc=$("$aws" ec2 describe-vpcs --filters Name=is-default,Values=true --query 'Vpcs[0].VpcId' --output text)
sg=$("$aws" ec2 describe-security-groups --filters Name=group-name,Values=yyc-sbx-proto --query 'SecurityGroups[0].GroupId' --output text)
if [ "$sg" = None ]; then
  sg=$("$aws" ec2 create-security-group --group-name yyc-sbx-proto --description "yyc sandbox prototype" --vpc-id "$vpc" \
    --tag-specifications 'ResourceType=security-group,Tags=[{Key=app,Value=yyc-ai-sandbox}]' --query GroupId --output text)
  # members talk to each other freely (migration stream, routed sandbox traffic); SSH from anywhere (key only);
  # WireGuard from anywhere (authenticated by key; the overlay to hosts outside AWS, overlay-up.sh)
  "$aws" ec2 authorize-security-group-ingress --group-id "$sg" --ip-permissions \
    "IpProtocol=-1,UserIdGroupPairs=[{GroupId=$sg}]" "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=udp,FromPort=51820,ToPort=51820,IpRanges=[{CidrIp=0.0.0.0/0}]" >/dev/null
fi
# one subnet (one AZ) for all three, so a /32 route to "the host the sandbox is on" is one hop
subnet=$("$aws" ec2 describe-subnets --filters Name=vpc-id,Values="$vpc" Name=default-for-az,Values=true \
  --query 'sort_by(Subnets,&AvailabilityZone)[0].SubnetId' --output text)
ami=$("$aws" ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id \
  --query Parameter.Value --output text)

launch() { # name type disk-GiB [extra args]
  local extra=("${@:4}")
  "$aws" ec2 run-instances "${extra[@]}" --image-id "$ami" --instance-type "$2" --key-name yyc-sbx-proto \
    --network-interfaces "DeviceIndex=0,SubnetId=$subnet,Groups=$sg,AssociatePublicIpAddress=true" \
    --block-device-mappings "DeviceName=/dev/sda1,Ebs={VolumeSize=$3,VolumeType=gp3,DeleteOnTermination=true}" \
    --instance-initiated-shutdown-behavior terminate --metadata-options HttpTokens=required,HttpPutResponseHopLimit=1 \
    --tag-specifications "${TAG/NAME/$1}" 'ResourceType=volume,Tags=[{Key=app,Value=yyc-ai-sandbox}]' \
    --query 'Instances[0].InstanceId' --output text
}
# a failed launch (quota, capacity) must not leave the others running unrecorded: terminate what this run started
started=()
trap '[ ${#started[@]} -gt 0 ] && "$aws" ec2 terminate-instances --instance-ids "${started[@]}" >/dev/null; rm -f "$STATE"/*.id.new' ERR
for n in a b egress; do
  [ -s "$STATE/$n.id" ] && continue
  if [ $n = egress ]; then launch egress "$EGRESS_TYPE" 12 > "$STATE/$n.id.new"
  else launch "host-$n" "$HOST_TYPE" 30 ${NESTED:+--cpu-options NestedVirtualization=enabled} > "$STATE/$n.id.new"; fi
  started+=("$(cat "$STATE/$n.id.new")")
done
for f in "$STATE"/*.id.new; do [ -e "$f" ] && mv "$f" "${f%.new}"; done
trap - ERR
ids=$(cat "$STATE"/{a,b,egress}.id)
"$aws" ec2 wait instance-running --instance-ids $ids
# hosts forward packets for sandbox addresses that aren't their own
for n in a b egress; do "$aws" ec2 modify-instance-attribute --instance-id "$(cat "$STATE/$n.id")" --no-source-dest-check; done
"$aws" ec2 describe-instances --instance-ids $ids --query 'Reservations[].Instances[].[Tags[?Key==`Name`]|[0].Value,InstanceId,PublicIpAddress,PrivateIpAddress,LaunchTime]' --output text | tee "$STATE/hosts.txt"

#!/usr/bin/env bash
# Terminates every prototype instance (tag app=yyc-ai-sandbox) and prints their run times for the cost record.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); aws="$here/aws.sh"
ids=$("$aws" ec2 describe-instances --filters Name=tag:app,Values=yyc-ai-sandbox Name=instance-state-name,Values=pending,running,stopping,stopped \
  --query 'Reservations[].Instances[].InstanceId' --output text)
[ -n "$ids" ] || { echo "nothing running"; exit 0; }
"$aws" ec2 describe-instances --instance-ids $ids --query 'Reservations[].Instances[].[InstanceId,InstanceType,LaunchTime]' --output text
"$aws" ec2 terminate-instances --instance-ids $ids --query 'TerminatingInstances[].[InstanceId,CurrentState.Name]' --output text
date -u +"terminated at %FT%TZ"

#!/usr/bin/env bash
# Runs `aws` with the YYC instance's AWS account (keys YYC_AWS_*), never the plain AWS_* keys.
# Region: $SBX_REGION, default eu-central-1 (Frankfurt), where the sandbox hosts run.
# Without YYC_AWS_* in the env it asks Jarvis 2's session-secret for them.
set -euo pipefail
if [ -n "${YYC_AWS_ACCESS_KEY_ID:-}" ]; then
  exec env -u AWS_PROFILE -u AWS_SESSION_TOKEN AWS_ACCESS_KEY_ID="$YYC_AWS_ACCESS_KEY_ID" AWS_SECRET_ACCESS_KEY="$YYC_AWS_SECRET_ACCESS_KEY" \
    AWS_DEFAULT_REGION="${SBX_REGION:-eu-central-1}" aws "$@"
fi
exec session-secret --keys YYC_AWS_ACCESS_KEY_ID,YYC_AWS_SECRET_ACCESS_KEY,YYC_AWS_DEFAULT_REGION --exec "$0" "$@"

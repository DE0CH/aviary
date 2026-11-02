#!/usr/bin/env bash
# Start the CI S3 server: versitygw (S3 gateway over a POSIX directory), pinned by digest in ci.yml. MinIO, which
# platform 6.1 names, no longer publishes anything to pull: docker.io/minio/minio is gone ("repository does not
# exist"), quay.io/minio/minio refuses anonymous pulls (401) and dl.min.io answers 410 (checked 2026-11-02). Credentials
# are generated per run. Prints the AVIARY_TEST_S3_* lines for $GITHUB_ENV.
#   s3.sh <image> <host port>
set -euo pipefail
image=$1
port=$2
data=$(mktemp -d)
chmod 0777 "$data"
access=$(openssl rand -hex 10)
secret=$(openssl rand -hex 20)
docker run -d --name aviary-ci-s3 -p "127.0.0.1:$port:7070" -v "$data:/data" "$image" \
  --access "$access" --secret "$secret" --region us-east-1 --port :7070 posix /data >/dev/null
for _ in $(seq 1 60); do
  if curl -s -o /dev/null "http://127.0.0.1:$port/"; then
    echo "AVIARY_TEST_S3_ENDPOINT=http://127.0.0.1:$port"
    echo "AVIARY_TEST_S3_REGION=us-east-1"
    echo "AVIARY_TEST_S3_ACCESS_KEY_ID=$access"
    echo "AVIARY_TEST_S3_SECRET_ACCESS_KEY=$secret"
    exit 0
  fi
  sleep 1
done
docker logs aviary-ci-s3 >&2
exit 1

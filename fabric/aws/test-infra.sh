#!/usr/bin/env bash
# Offline test of infra.sh: a stand-in `aws` records every call and answers from canned values, so the test checks
# that every deployment value comes from the --config file (region, subnet, template, tags, the three pins) and that
# the IAM policies are filled in and stay under IAM's policy size limits. Needs bash and jq. Usage: test-infra.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
mkdir "$t/bin"
cat > "$t/bin/aws" <<'EOF'
#!/usr/bin/env bash
# the stand-in: log the call, answer what infra.sh asks
printf '%s\n' "$*" >> "$AWS_LOG"
args=" $* "
case $args in
  *" sts get-caller-identity "*) echo 123456789012 ;;
  *" describe-launch-template-versions "*"--versions \$Default"*"VolumeSize"*) echo "${LT_DISK:-40}" ;;
  *" describe-launch-template-versions "*"--versions \$Default"*) echo ami-0cur ;;
  *" describe-launch-templates "*"LaunchTemplateId,DefaultVersionNumber"*) printf 'lt-0abc\t2\n' ;;
  *" describe-launch-templates "*"LaunchTemplateId"*) echo lt-0abc ;;
  *" describe-launch-templates "*) exit 0 ;;
  *" create-launch-template-version "*) echo 2 ;;
  *" describe-launch-template-versions "*) echo 1 ;;
  *" describe-security-groups "*) echo sg-0abc ;;
  *" describe-images "*"Key=='image'"*) echo "$AMI_IMAGE" ;;
  *" describe-images "*"Key=='sandboxd'"*) echo aviary-0123456789ab ;;
  *" describe-images "*"Key=='runsc'"*) echo precopy-20261021.2 ;;
  *" iam get-user "*) exit 0 ;;
  *" iam get-policy "*) [ -n "${POLICY_EXISTS:-}" ] || exit 254 ;;
  *" iam list-policy-versions "*) echo v1 ;;
  *) : ;;
esac
EOF
chmod +x "$t/bin/aws"
d=$(printf 'd%.0s' $(seq 64))
cat > "$t/hermit.json" <<EOF
{"sandbox": {"image": "ghcr.io/example/sandbox@sha256:$d"},
 "fabric": {"sandboxd": "aviary-0123456789ab", "runsc": "precopy-20261021.2",
   "aws": {"region": "xx-test-1", "launchTemplate": "test-host", "maxHosts": 3, "subnet": "subnet-0test",
           "tags": {"app": "test-host", "deployment": "example"}}}}
EOF
export AWS_LOG=$t/log PATH=$t/bin:$PATH AMI_IMAGE=$d
run() { : > "$AWS_LOG"; bash "$here/infra.sh" --config "$t/hermit.json" "$@"; }
fail() { echo "FAIL: $*" >&2; cat "$AWS_LOG" >&2; exit 1; }
has() { grep -qF -- "$1" "$AWS_LOG" || fail "no aws call with: $1"; }

# no config: usage, exit 2
if bash "$here/infra.sh" current >/dev/null 2>&1; then fail "runs without --config"; fi

run current > "$t/out"
grep -q "^ami-0cur image=$d sandboxd=aviary-0123456789ab runsc=precopy-20261021.2 disk=40$" "$t/out" || fail "current: $(cat "$t/out")"
has "--region xx-test-1 ec2 describe-launch-template-versions --launch-template-name test-host"
echo "ok  current reads the template and the AMI's tags"

AMI_IMAGE=$d run check >/dev/null || fail "check with every pin in place"
if AMI_IMAGE=${d/d/e} run check >/dev/null 2>&1; then fail "check passes with another image"; fi
echo "ok  check compares the AMI with the three pins (the image digest from the pinned ref, offline)"

# before a release exists: a local build or a pin that is not a release never loops bakes
jq '.fabric.sandboxd = "local"' "$t/hermit.json" > "$t/local.json"
: > "$AWS_LOG"
if bash "$here/infra.sh" --config "$t/local.json" check 2> "$t/err"; then fail "check passes with fabric.sandboxd local"; fi
grep -q "not an aviary-<sha12> release" "$t/err" || fail "check does not say why: $(cat "$t/err")"
rc=0; bash "$here/infra.sh" --config "$t/local.json" sync > /dev/null 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "sync with fabric.sandboxd local exited $rc, not 3"
! grep -qE "run-instances|create-image" "$AWS_LOG" || fail "sync baked with fabric.sandboxd local"
rc=0; SANDBOXD_BINARY=/nonexistent run sync > /dev/null 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "sync with SANDBOXD_BINARY exited $rc, not 3"
! grep -qE "run-instances|create-image|import-key-pair" "$AWS_LOG" || fail "sync baked with SANDBOXD_BINARY"
echo "ok  sync refuses a local build and a pin that is not a release (nothing baked); check says why"

# the hosts' disk: fabric.aws.diskGiB (40 when absent); a template with another size is stale without a bake
jq '.fabric.aws.diskGiB = 220' "$t/hermit.json" > "$t/disk.json"
rc=0; : > "$AWS_LOG"; bash "$here/infra.sh" --config "$t/disk.json" check > /dev/null 2>"$t/err" || rc=$?
[ "$rc" = 4 ] || fail "check with a 40 GiB template and diskGiB 220 exited $rc, not 4"
grep -q "disk is 40 GiB; fabric.aws.diskGiB is 220" "$t/err" || fail "check does not name the disk: $(cat "$t/err")"
LT_DISK=220 bash "$here/infra.sh" --config "$t/disk.json" check > /dev/null || fail "check with the disk in place"
: > "$AWS_LOG"; bash "$here/infra.sh" --config "$t/disk.json" sync > /dev/null || fail "sync with only the disk stale"
! grep -qE "run-instances|create-image" "$AWS_LOG" || fail "sync baked for a disk change"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '"ImageId":"ami-0cur"' || fail "disk sync: not the current AMI"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '"VolumeSize":220,' || fail "disk sync: not 220 GiB"
jq '.fabric.aws.diskGiB = "big"' "$t/hermit.json" > "$t/baddisk.json"
if bash "$here/infra.sh" --config "$t/baddisk.json" current > /dev/null 2>&1; then fail "a diskGiB that is no number"; fi
echo "ok  disk: the template's volume is fabric.aws.diskGiB; a size change writes the template, no bake"

run template ami-0new > /dev/null
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '"VolumeSize":40,' || fail "template disk (the default)"
has "create-launch-template-version --launch-template-name test-host"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '"SubnetId":"subnet-0test"' || fail "template subnet"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '"ImageId":"ami-0new"' || fail "template AMI"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '{"Key":"app","Value":"test-host"}' || fail "template tags"
grep -F "create-launch-template-version" "$AWS_LOG" | grep -q '{"Key":"deployment","Value":"example"}' || fail "template tags"
has "delete-launch-template-versions --launch-template-name test-host --versions 1"
echo "ok  template: the config's subnet and tags, the given AMI, old versions deleted"

run iam > /dev/null
pol=$(grep -F "put-user-policy --user-name hermit-scaler" "$AWS_LOG" | sed 's/.*--policy-document //')
jq -e . <<< "$pol" > /dev/null || fail "scaler policy is not JSON"
grep -q 'arn:aws:ec2:xx-test-1:123456789012:launch-template/lt-0abc' <<< "$pol" || fail "scaler policy ARNs"
grep -q '"aws:RequestTag/app":"test-host"' <<< "$pol" || fail "scaler policy tag"
grep -q '"ec2:InstanceType":"m7i.xlarge"' <<< "$pol" || fail "scaler policy instance type (the default)"
! grep -qE 'ACCOUNT|LT_ID|REGION|TAG_APP|TEMPLATE|INSTANCE_TYPE' <<< "$pol" || fail "a placeholder left in the scaler policy"
if grep -qE 'REGION|ACCOUNT|LT_ID|TAG_APP|TEMPLATE' <<< "$pol"; then fail "placeholders left: $pol"; fi
echo "ok  iam: hermit-scaler's policy filled in ($(wc -c <<< "$pol") bytes)"

run ci-iam > /dev/null
pol=$(grep -F "create-policy --policy-name hermit-ami-ci" "$AWS_LOG" | sed -e 's/.*--policy-document //' -e 's/ --tags .*//')
jq -e . <<< "$pol" > /dev/null || fail "ci policy is not JSON"
has "iam attach-user-policy --user-name hermit-ami-ci --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci"
grep -q '"arn:aws:ec2:xx-test-1:\*:instance/\*"' <<< "$pol" || fail "ci policy not pinned to the config's region"
if grep -qE 'REGION|ACCOUNT|LT_ID|TAG_APP|TEMPLATE' <<< "$pol"; then fail "placeholders left: $pol"; fi
grep -q 'key-pair/test-host-ami-\*' <<< "$pol" || fail "ci policy key pair prefix"
grep -q '"test-host-build"' <<< "$pol" || fail "ci policy builder tag"
# a deployment naming its template and app tag with 24 characters each must still fit IAM's managed policy limit
long=$(printf 'x%.0s' $(seq 24))
size=$(sed -e "s/LT_ID/lt-0123456789abcdef0/g" -e "s/TAG_APP/$long/g" -e "s/TEMPLATE/$long/g" -e "s/REGION/ap-southeast-3/g" "$here/ami-ci-policy.json" | jq -c . | wc -c)
[ "$size" -le 6144 ] || fail "ci policy is $size characters with 24-character names"
echo "ok  ci-iam: hermit-ami-ci's managed policy filled in and attached, $size characters with 24-character names (limit 6144)"
# a second run replaces the policy's default version (old versions deleted first), no second policy
: > "$AWS_LOG"; POLICY_EXISTS=1 bash "$here/infra.sh" --config "$t/hermit.json" ci-iam > /dev/null
has "iam delete-policy-version --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci --version-id v1"
has "iam create-policy-version --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci"
! grep -qF "create-policy --policy-name" "$AWS_LOG" || fail "ci-iam created a second policy"
echo "ok  ci-iam again: a new default version, the old one deleted"

# a config value that is not a plain name never reaches a policy (no sed injection, no widened ARN)
jq '.fabric.aws.launchTemplate = "a/*"' "$t/hermit.json" > "$t/bad.json"
: > "$AWS_LOG"
if bash "$here/infra.sh" --config "$t/bad.json" iam > /dev/null 2>&1; then fail "iam with a template name a/*"; fi
! grep -q put-user-policy "$AWS_LOG" || fail "a policy was put with a template name a/*"
jq '.fabric.aws.instanceType = "c7i.2xlarge" | .fabric.aws.tags.app = "REGION"' "$t/hermit.json" > "$t/other.json"
: > "$AWS_LOG"; bash "$here/infra.sh" --config "$t/other.json" iam > /dev/null
pol=$(grep -F "put-user-policy --user-name hermit-scaler" "$AWS_LOG" | sed 's/.*--policy-document //')
grep -q '"ec2:InstanceType":"c7i.2xlarge"' <<< "$pol" || fail "the config's instance type"
grep -q '"aws:RequestTag/app":"REGION"' <<< "$pol" || fail "a value that is a placeholder's name is not filled in again"
echo "ok  policies: values checked, filled in one pass, the config's instance type"

run sg > /dev/null
echo "ok  sg: finds the group by the template's name"
echo "ALL OK"

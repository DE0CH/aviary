#!/usr/bin/env bash
# One-shot Jobs of the switch (hermit-jfs-owner, A2 and A6):
# applied by hand on the new cluster, never by yyc-ai.yml. A Job is immutable, so one in yyc/deploy/ blocked every
# re-pin of the image tags until someone added Deploy-Replace/Deploy-Prune trailers; here each runs on the images
# yyc/deploy/kustomization.yaml pins now, outside yyc-apply's inventory.
#   KUBECONFIG=<new cluster operator kubeconfig> yyc/oneshot/run.sh <file.yaml> [--replace]
# Prints the Job names it created. A Job of the same name that exists is kept (exit 1) unless --replace deletes it
# first (its pods and logs go with it: record what it printed before).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
file=${1:?usage: run.sh <file.yaml> [--replace]}
replace=${2:-}
kust="$here/../deploy/kustomization.yaml"
tag() { sed -n "s/.*{name: $1, newName: \([^,]*\), newTag: \"\([^\"]*\)\"}.*/\1:\2/p" "$kust" | head -1; }
rendered=$(sed -E \
  -e "s#^( *image: )magpie\$#\1$(tag magpie)#" \
  -e "s#^( *image: )hermit\$#\1$(tag hermit)#" \
  "$file")
if grep -qE '^ *image: (magpie|hermit)$' <<<"$rendered"; then echo "run.sh: an image was not pinned" >&2; exit 1; fi
# a ConfigMap of a kustomize generator has a hash suffix only the render adds: the one on the cluster
for cm in $(sed -n 's/.*configMap: {name: \([a-z0-9-]*\)}.*/\1/p' <<<"$rendered" | sort -u); do
  ns=$(sed -n 's/^  namespace: //p' <<<"$rendered" | head -1)
  kubectl -n "$ns" get configmap "$cm" >/dev/null 2>&1 && continue
  real=$(kubectl -n "$ns" get configmap -o name | sed -n "s#^configmap/\($cm-[a-z0-9]\{10\}\)\$#\1#p" | tail -1)
  [ -n "$real" ] || { echo "run.sh: no ConfigMap $cm in $ns" >&2; exit 1; }
  rendered=$(sed "s#configMap: {name: $cm}#configMap: {name: $real}#" <<<"$rendered")
done
for j in $(python3 -c 'import sys,yaml; [print(d["metadata"].get("namespace","default")+"/"+d["metadata"]["name"]) for d in yaml.safe_load_all(sys.stdin) if d and d.get("kind")=="Job"]' <<<"$rendered"); do
  ns=${j%%/*}; name=${j#*/}
  if kubectl -n "$ns" get job "$name" >/dev/null 2>&1; then
    if [ "$replace" = --replace ]; then kubectl -n "$ns" delete job "$name" --wait=true >/dev/null
    else echo "run.sh: job $ns/$name exists (record its output, then --replace)" >&2; exit 1; fi
  fi
done
kubectl apply -f - <<<"$rendered" | sed -n 's#^job.batch/\(.*\) created#\1#p;s#^\(.*\) \(created\|configured\|unchanged\)$#\1 \2#p'

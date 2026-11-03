#!/bin/bash
# `apt-get` and `apt` in the sandbox (installed as /usr/local/bin/apt-get, /usr/local/bin/apt: ahead of /usr/bin on
# PATH and on sudo's secure_path; hermit spec 17, A5.F4).
# - uid 1000 goes through passwordless sudo (root exists only inside the sandbox), so `apt-get install -y x` and
#   `sudo apt-get install -y x` both work; without sudo (no-new-privileges) a mutating command exits 100 and says why;
# - the image ships without package lists: an install, upgrade or build-dep fetches them first;
# - apt reaches the mirrors through the sandbox's proxy (sudo keeps the proxy variables; the supervisor also writes
#   /etc/apt/apt.conf.d/80sandbox-proxy at its first start, supervisor spec 4.3);
# - never a prompt: DEBIAN_FRONTEND defaults to noninteractive here (the supervisor passes no image environment
#   beyond PATH, LANG, LC_ALL and HOME to the agent, supervisor spec 6.2).
export DEBIAN_FRONTEND=${DEBIAN_FRONTEND:-noninteractive}
name=$(basename "$0")
real=/usr/bin/$name
args=("$@")

# the subcommand: the first argument that is not an option (or an option's value)
sub=
i=0
while [ $i -lt ${#args[@]} ]; do
  a=${args[$i]}
  case $a in
    -o|-c|-t|-a|--option|--config-file|--target-release|--default-release|--host-architecture) i=$((i+2)); continue ;;
    -*) i=$((i+1)); continue ;;
  esac
  sub=$a
  break
done
case $sub in
  install|reinstall|remove|purge|autoremove|upgrade|full-upgrade|dist-upgrade|build-dep|update|satisfy|clean|autoclean|edit-sources|source|download|changelog) mutating=1 ;;
  *) mutating=0 ;;
esac

if [ "$(id -u)" != 0 ] && [ $mutating = 1 ]; then
  if sudo -n true 2>/dev/null; then exec sudo -n "$0" "$@"; fi
  case $sub in
    source|download|changelog) ;;  # these work without root
    *)
      echo "$name: installing system packages needs root, which this sandbox does not have." >&2
      echo "Use pip (Python), npm -g (Node) or an installer that puts tools into ~/.local instead." >&2
      exit 100 ;;
  esac
fi

if [ "$(id -u)" = 0 ]; then
  case $sub in
    install|reinstall|upgrade|full-upgrade|dist-upgrade|build-dep|satisfy|source)
      if ! compgen -G '/var/lib/apt/lists/*_Packages*' >/dev/null; then
        echo "$name: fetching the package lists first (apt-get update)" >&2
        "$real" update -qq || echo "$name: apt-get update failed; continuing" >&2
      fi ;;
  esac
fi
exec "$real" "$@"

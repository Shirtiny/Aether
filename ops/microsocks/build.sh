#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
caller_dir=$PWD
work_dir=${1:-$(mktemp -d -t aether-microsocks-build.XXXXXX)}
output=${2:-$caller_dir/aether-microsocks-1.0.5-aether1}

case "$work_dir" in
  /*) ;;
  *) work_dir="$caller_dir/$work_dir" ;;
esac
case "$output" in
  /*) ;;
  *) output="$caller_dir/$output" ;;
esac

mkdir -p "$work_dir" "$(dirname -- "$output")"
if find "$work_dir" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
  echo "build directory must be empty: $work_dir" >&2
  exit 1
fi

cd "$work_dir"
apt-get source microsocks=1.0.5-1

printf '%s  %s\n' \
  939d1851a18a4c03f3cc5c92ff7a50eaf045da7814764b4cb9e26921db15abc8 \
  microsocks_1.0.5.orig.tar.gz | sha256sum --check --strict
printf '%s  %s\n' \
  f9d49b78cc483cd9287b7cffbd5cffe083d29d916736531df19f5684b717130d \
  microsocks_1.0.5-1.debian.tar.xz | sha256sum --check --strict

patch --batch --forward -p0 \
  < "$script_dir/patches/microsocks-1.0.5-aether-framing.patch"

cd microsocks-1.0.5
make clean
export DEB_BUILD_MAINT_OPTIONS='hardening=+all'
make -j2 \
  CC=cc \
  CPPFLAGS="$(dpkg-buildflags --get CPPFLAGS) -DAETHER_MICROSOCKS_FRAMING_FIX=1" \
  CFLAGS="$(dpkg-buildflags --get CFLAGS) -Werror" \
  LDFLAGS="$(dpkg-buildflags --get LDFLAGS)"

temporary_output="$output.tmp.$$"
trap 'rm -f -- "$temporary_output"' EXIT
install -m 0755 microsocks "$temporary_output"
strip --strip-unneeded "$temporary_output"
mv -- "$temporary_output" "$output"
trap - EXIT

sha256sum "$output"
printf 'built=%s\nsource=%s\n' "$output" "$work_dir/microsocks-1.0.5"

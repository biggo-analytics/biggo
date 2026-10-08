#!/bin/sh
# Installs biggo from its GitHub releases: downloads the archive for this system, checks it
# against the published checksums, and puts the executable in a directory.
#
#   curl -fsSL https://raw.githubusercontent.com/biggo-analytics/biggo/main/install.sh | sh
#
# Settings, as environment variables:
#   BIGGO_VERSION   the release to install, such as v0.2.0 (default: the latest)
#   BIGGO_INSTALL   the directory to install into (default: ~/.local/bin)
#   BIGGO_DOWNLOAD  where the releases are (default: the releases of the GitHub repository)
set -eu

releases="${BIGGO_DOWNLOAD:-https://github.com/biggo-analytics/biggo/releases}"
version="${BIGGO_VERSION:-}"
into="${BIGGO_INSTALL:-$HOME/.local/bin}"

fail() {
  echo "install.sh: $1" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) system="apple-darwin" ;;
  Linux) system="unknown-linux-gnu" ;;
  *) fail "there is no prebuilt biggo for $(uname -s); see the README for building from source" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) machine="aarch64" ;;
  x86_64 | amd64) machine="x86_64" ;;
  *) fail "there is no prebuilt biggo for $(uname -m); see the README for building from source" ;;
esac
name="biggo-$machine-$system"

if [ -n "$version" ]; then
  from="$releases/download/$version"
else
  from="$releases/latest/download"
fi

command -v curl > /dev/null || fail "curl is needed to download biggo"
if command -v sha256sum > /dev/null; then
  digest() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum > /dev/null; then
  digest() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  fail "sha256sum or shasum is needed to check the download"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "downloading $from/$name.tar.gz"
curl -fsSL "$from/$name.tar.gz" -o "$work/$name.tar.gz" \
  || fail "cannot download $from/$name.tar.gz"
curl -fsSL "$from/SHA256SUMS" -o "$work/SHA256SUMS" \
  || fail "cannot download $from/SHA256SUMS"

wanted="$(awk -v file="$name.tar.gz" '$2 == file || $2 == "*" file { print $1 }' "$work/SHA256SUMS")"
[ -n "$wanted" ] || fail "SHA256SUMS has no line for $name.tar.gz"
found="$(digest "$work/$name.tar.gz")"
[ "$wanted" = "$found" ] || fail "the download does not match its checksum; nothing was installed"

tar -xzf "$work/$name.tar.gz" -C "$work"
mkdir -p "$into"
# Moved into place in one step, so a biggo that is running is never half replaced.
cp "$work/$name/biggo" "$into/.biggo.new"
chmod 755 "$into/.biggo.new"
mv -f "$into/.biggo.new" "$into/biggo"

echo "installed $("$into/biggo" version 2> /dev/null || echo biggo) in $into"
case ":$PATH:" in
  *":$into:"*) ;;
  *) echo "$into is not on your PATH; add it to run biggo by name" ;;
esac

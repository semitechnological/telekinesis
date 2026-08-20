#!/usr/bin/env bash
set -euo pipefail

REPO="${TK_REPO:-tschk/telekinesis}"
DEST_DIR="${TK_INSTALL_DIR:-${HOME}/.local/bin}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "install.sh: missing required command: $1" >&2
    exit 1
  }
}

need curl
need tar
need mktemp
need uname

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux) os_tag="unknown-linux-gnu" ;;
  Darwin) os_tag="apple-darwin" ;;
  *)
    echo "install.sh: unsupported OS: $os (linux and mac only)" >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64 | amd64) arch_tag="x86_64" ;;
  arm64 | aarch64) arch_tag="aarch64" ;;
  *)
    echo "install.sh: unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

target="${arch_tag}-${os_tag}"
asset="tk-${target}.tar.gz"
base="https://github.com/${REPO}/releases/latest/download"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

curl -fsSL -o "${tmpdir}/${asset}" "${base}/${asset}"
curl -fsSL -o "${tmpdir}/checksums.txt" "${base}/checksums.txt"

verify_checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    grep " ${asset}$" checksums.txt | sha256sum -c -
  elif command -v shasum >/dev/null 2>&1; then
    grep " ${asset}$" checksums.txt | shasum -a 256 -c -
  else
    echo "install.sh: need sha256sum or shasum to verify ${asset}" >&2
    exit 1
  fi
}

(
  cd "$tmpdir"
  if ! grep -q " ${asset}$" checksums.txt; then
    echo "install.sh: ${asset} is not listed in checksums.txt" >&2
    exit 1
  fi
  verify_checksum
)

tar -C "$tmpdir" -xzf "${tmpdir}/${asset}"
if [ ! -f "${tmpdir}/tk" ]; then
  echo "install.sh: archive ${asset} did not contain tk" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
if command -v install >/dev/null 2>&1; then
  install -m 755 "${tmpdir}/tk" "${DEST_DIR}/tk"
else
  cp "${tmpdir}/tk" "${DEST_DIR}/tk"
  chmod 755 "${DEST_DIR}/tk"
fi

echo "installed ${DEST_DIR}/tk (${target})"
case ":${PATH}:" in
  *":${DEST_DIR}:"*) ;;
  *)
    echo "add ${DEST_DIR} to PATH to run tk" >&2
    ;;
esac

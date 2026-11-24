#!/usr/bin/env sh
# Tinkr per-user installer (macOS / Linux). Verifies SHA-256 before installing.
set -eu

VERSION="${TINKR_VERSION:-0.1.0}"
REPO="${TINKR_REPO:-thunderkex/tinkr}"
INSTALL_DIR="${TINKR_INSTALL_DIR:-$HOME/.local/bin}"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH="x64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

ASSET="tinkr-${VERSION}-${OS}-${ARCH}.tar.gz"
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "${BASE}/${ASSET}" -o "${TMP}/${ASSET}"
curl -fsSL "${BASE}/SHA256SUMS" -o "${TMP}/SHA256SUMS"

EXPECTED="$(tr -d '\r' < "${TMP}/SHA256SUMS" | grep " ${ASSET}\$" | awk '{print $1}')"
[ -n "$EXPECTED" ] || { echo "Checksum entry missing for ${ASSET}" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL="$(sha256sum "${TMP}/${ASSET}" | awk '{print $1}')"
else
  ACTUAL="$(shasum -a 256 "${TMP}/${ASSET}" | awk '{print $1}')"
fi
[ "$EXPECTED" = "$ACTUAL" ] || { echo "SHA-256 mismatch!" >&2; exit 1; }

mkdir -p "$INSTALL_DIR"
tar -xzf "${TMP}/${ASSET}" -C "$TMP"
install -m 0755 "${TMP}/tinkr-${VERSION}-${OS}-${ARCH}/tinkr" "${INSTALL_DIR}/tinkr"
install -m 0755 "${TMP}/tinkr-${VERSION}-${OS}-${ARCH}/tinkr-proxy" "${INSTALL_DIR}/tinkr-proxy"
echo "Installed tinkr ${VERSION} to ${INSTALL_DIR}. Run 'tinkr' to auto-setup."

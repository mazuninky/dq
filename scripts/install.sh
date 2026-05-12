#!/bin/sh
# install.sh — POSIX installer for `dq` (https://github.com/mazuninky/dq).
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh
#   sh install.sh --version v2026.20.1 --install-dir ~/.local/bin
#
# Detects OS + arch, downloads the matching prebuilt tarball from a GitHub
# Release, verifies its SHA256 against the published `dq-checksums.txt`,
# extracts the binary, and installs it. No Rust toolchain required.

set -eu

# -----------------------------------------------------------------------------
# Defaults
# -----------------------------------------------------------------------------

REPO="${DQ_REPO:-mazuninky/dq}"
VERSION="${DQ_VERSION:-latest}"
INSTALL_DIR="${DQ_INSTALL_DIR:-}"
NO_MODIFY_PATH="0"
TMP_DIR=""

# -----------------------------------------------------------------------------
# Cleanup trap
# -----------------------------------------------------------------------------

cleanup() {
    if [ -n "${TMP_DIR}" ] && [ -d "${TMP_DIR}" ]; then
        rm -rf "${TMP_DIR}"
    fi
}
trap cleanup EXIT INT TERM HUP

# -----------------------------------------------------------------------------
# Logging helpers (write to stderr so curl|sh doesn't pipe them onward)
# -----------------------------------------------------------------------------

log() { printf 'install.sh: %s\n' "$*" >&2; }
die() { printf 'install.sh: error: %s\n' "$*" >&2; exit 1; }

# -----------------------------------------------------------------------------
# Usage
# -----------------------------------------------------------------------------

usage() {
    cat >&2 <<'EOF'
Usage: install.sh [options]

Options:
  --version <VER>      Release tag to install (default: latest, e.g. v2026.20.1).
  --install-dir <DIR>  Install destination (default: ~/.local/bin for non-root,
                       /usr/local/bin for root).
  --repo <OWNER/NAME>  GitHub repo to install from (default: mazuninky/dq).
  --no-modify-path     Skip the "add INSTALL_DIR to PATH" reminder.
  -h, --help           Show this help and exit.

Environment overrides:
  DQ_VERSION, DQ_INSTALL_DIR, DQ_REPO mirror the matching flags.
EOF
}

# -----------------------------------------------------------------------------
# Argument parsing
# -----------------------------------------------------------------------------

require_value() {
    # $1 = flag name (for the error), $2 = candidate value (may be unset / "-…").
    [ -n "${2:-}" ] || die "$1 requires a value (try --help)"
    case "$2" in
        -*) die "$1 requires a value, got option-like '$2' (try --help)" ;;
    esac
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            require_value "--version" "${2:-}"
            VERSION="$2"; shift 2 ;;
        --install-dir)
            require_value "--install-dir" "${2:-}"
            INSTALL_DIR="$2"; shift 2 ;;
        --repo)
            require_value "--repo" "${2:-}"
            REPO="$2"; shift 2 ;;
        --no-modify-path) NO_MODIFY_PATH="1"; shift ;;
        -h|--help)        usage; exit 0 ;;
        *)                die "unknown option: $1 (try --help)" ;;
    esac
done

# -----------------------------------------------------------------------------
# Tool discovery
# -----------------------------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

if have curl; then
    DOWNLOADER="curl"
elif have wget; then
    DOWNLOADER="wget"
else
    die "neither curl nor wget found on PATH"
fi

if have sha256sum; then
    SHA="sha256sum"
elif have shasum; then
    SHA="shasum -a 256"
elif have openssl; then
    SHA="openssl-256"
else
    die "no SHA256 tool found (need sha256sum, shasum, or openssl)"
fi

if ! have tar; then
    die "tar is required to extract the release archive"
fi

# -----------------------------------------------------------------------------
# Target detection
# -----------------------------------------------------------------------------

detect_target() {
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "${arch}" in
        x86_64|amd64)        arch="x86_64" ;;
        aarch64|arm64)       arch="aarch64" ;;
        *) die "unsupported architecture: ${arch}" ;;
    esac

    case "${os}" in
        linux)   echo "${arch}-unknown-linux-gnu" ;;
        darwin)
            if [ "${arch}" = "aarch64" ]; then
                echo "aarch64-apple-darwin"
            else
                # x86_64-apple-darwin builds aren't shipped in M6 — Apple Silicon
                # is the macOS minimum. Bail with a clear note.
                die "x86_64-apple-darwin is not in the M6 release matrix; install via 'cargo install dq' instead"
            fi ;;
        mingw*|msys*|cygwin*)
            if [ "${arch}" = "x86_64" ]; then
                echo "x86_64-pc-windows-msvc"
            else
                die "Windows on ${arch} is not in the M6 release matrix"
            fi ;;
        *) die "unsupported OS: ${os}" ;;
    esac
}

TARGET="$(detect_target)"
log "detected target: ${TARGET}"

# -----------------------------------------------------------------------------
# Install dir defaults
# -----------------------------------------------------------------------------

if [ -z "${INSTALL_DIR}" ]; then
    if [ "$(id -u)" = "0" ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
    fi
fi

# -----------------------------------------------------------------------------
# Resolve version
# -----------------------------------------------------------------------------

http_get() {
    url="$1"; dest="$2"
    case "${DOWNLOADER}" in
        curl) curl -fsSL --retry 3 -o "${dest}" "${url}" ;;
        wget) wget -q -O "${dest}" "${url}" ;;
    esac
}

http_get_to_stdout() {
    url="$1"
    case "${DOWNLOADER}" in
        curl) curl -fsSL --retry 3 "${url}" ;;
        wget) wget -q -O - "${url}" ;;
    esac
}

resolve_latest_version() {
    # GitHub redirects /releases/latest to /releases/tag/<TAG>; capture the redirect Location.
    case "${DOWNLOADER}" in
        curl)
            tag="$(curl -fsSI "https://github.com/${REPO}/releases/latest" \
                | awk -F': ' 'tolower($1) == "location" { print $2 }' \
                | tr -d '\r\n' \
                | sed -e 's|.*/tag/||')"
            ;;
        wget)
            tag="$(wget --max-redirect=0 -q -S "https://github.com/${REPO}/releases/latest" 2>&1 \
                | awk '/Location:/ { print $2 }' \
                | tail -n1 \
                | sed -e 's|.*/tag/||')"
            ;;
    esac
    [ -n "${tag}" ] || die "could not resolve latest version from GitHub"
    echo "${tag}"
}

if [ "${VERSION}" = "latest" ]; then
    VERSION="$(resolve_latest_version)"
fi
case "${VERSION}" in
    v*) ;;
    *)  VERSION="v${VERSION}" ;;
esac
log "installing version: ${VERSION}"

# -----------------------------------------------------------------------------
# Download + verify
# -----------------------------------------------------------------------------

case "${TARGET}" in
    *windows*) ARCHIVE_EXT="zip" ;;
    *)         ARCHIVE_EXT="tar.gz" ;;
esac

ARCHIVE_NAME="dq-${VERSION}-${TARGET}.${ARCHIVE_EXT}"
CHECKSUMS_NAME="dq-checksums.txt"
ARCHIVE_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE_NAME}"
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/${CHECKSUMS_NAME}"

TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'dq-install')"

log "downloading ${ARCHIVE_NAME}"
http_get "${ARCHIVE_URL}" "${TMP_DIR}/${ARCHIVE_NAME}" \
    || die "failed to download ${ARCHIVE_URL}"

log "downloading ${CHECKSUMS_NAME}"
http_get "${CHECKSUMS_URL}" "${TMP_DIR}/${CHECKSUMS_NAME}" \
    || die "failed to download ${CHECKSUMS_URL}"

verify_sha256() {
    archive="$1"; checksums="$2"
    expected="$(awk -v name="$(basename "${archive}")" \
                    '$2 == name || $2 == "*" name { print $1 }' "${checksums}")"
    [ -n "${expected}" ] || die "no checksum entry for $(basename "${archive}") in ${checksums}"
    case "${SHA}" in
        sha256sum)        actual="$(sha256sum "${archive}" | awk '{ print $1 }')" ;;
        "shasum -a 256")  actual="$(shasum -a 256 "${archive}" | awk '{ print $1 }')" ;;
        openssl-256)      actual="$(openssl dgst -sha256 "${archive}" | awk '{ print $NF }')" ;;
    esac
    [ "${expected}" = "${actual}" ] \
        || die "SHA256 mismatch: expected ${expected}, got ${actual}"
    log "checksum verified"
}

verify_sha256 "${TMP_DIR}/${ARCHIVE_NAME}" "${TMP_DIR}/${CHECKSUMS_NAME}"

# -----------------------------------------------------------------------------
# Extract
# -----------------------------------------------------------------------------

log "extracting"
case "${ARCHIVE_EXT}" in
    tar.gz)
        ( cd "${TMP_DIR}" && tar -xzf "${ARCHIVE_NAME}" )
        ;;
    zip)
        if have unzip; then
            ( cd "${TMP_DIR}" && unzip -q "${ARCHIVE_NAME}" )
        else
            die "unzip is required to extract Windows archives"
        fi
        ;;
esac

case "${TARGET}" in
    *windows*) BIN_NAME="dq.exe" ;;
    *)         BIN_NAME="dq" ;;
esac

# Locate the extracted binary — vendored layouts vary slightly.
EXTRACTED_BIN=""
for candidate in \
    "${TMP_DIR}/${BIN_NAME}" \
    "${TMP_DIR}/dq-${VERSION}-${TARGET}/${BIN_NAME}" \
    "${TMP_DIR}/dq/${BIN_NAME}"
do
    if [ -f "${candidate}" ]; then
        EXTRACTED_BIN="${candidate}"
        break
    fi
done
[ -n "${EXTRACTED_BIN}" ] || die "could not find ${BIN_NAME} inside ${ARCHIVE_NAME}"

# -----------------------------------------------------------------------------
# Install
# -----------------------------------------------------------------------------

mkdir -p "${INSTALL_DIR}" || die "could not create ${INSTALL_DIR}"
DEST="${INSTALL_DIR}/${BIN_NAME}"
mv "${EXTRACTED_BIN}" "${DEST}" || die "could not move binary to ${DEST}"
chmod +x "${DEST}"

# -----------------------------------------------------------------------------
# Self-test
# -----------------------------------------------------------------------------

if [ -x "${DEST}" ]; then
    if "${DEST}" --version >/dev/null 2>&1; then
        log "installed: $(${DEST} --version)"
    else
        log "warning: ${DEST} did not respond to --version (proceed with caution)"
    fi
fi

# -----------------------------------------------------------------------------
# PATH check
# -----------------------------------------------------------------------------

if [ "${NO_MODIFY_PATH}" = "0" ]; then
    case ":${PATH:-}:" in
        *:${INSTALL_DIR}:*)
            ;;
        *)
            log "note: ${INSTALL_DIR} is not on your PATH."
            log "      Add this line to ~/.bashrc, ~/.zshrc, or your shell rc:"
            log "        export PATH=\"${INSTALL_DIR}:\$PATH\""
            ;;
    esac
fi

log "done. Run \`dq --help\` to get started."

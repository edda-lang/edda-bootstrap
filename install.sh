#!/usr/bin/env bash
# Install the Edda bootstrap compiler: download the latest release for
# this platform from GitHub Releases, unpack it, and add it to PATH.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/edda-lang/edda-bootstrap/main/install.sh | bash
#
# Env overrides:
#   EDDA_INSTALL_DIR   install root (default: $HOME/.edda-bootstrap)
#   EDDA_INSTALL_TAG   release tag to install instead of "latest"

set -euo pipefail

REPO="edda-lang/edda-bootstrap"
INSTALL_DIR="${EDDA_INSTALL_DIR:-$HOME/.edda-bootstrap}"
TAG="${EDDA_INSTALL_TAG:-latest}"

log() { printf '%s\n' "$*" >&2; }
die() { log "install.sh: error: $*"; exit 1; }

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os-$arch" in
        Linux-x86_64) echo "x86-64-linux-gnu" ;;
        Linux-aarch64|Linux-arm64) echo "aarch64-linux-gnu" ;;
        Darwin-arm64) echo "aarch64-macos-darwin" ;;
        Darwin-x86_64)
            die "no x86_64 macOS release is published yet; Apple Silicon (aarch64-macos-darwin) only" ;;
        *) die "unsupported platform: $os $arch" ;;
    esac
}

release_json_url() {
    if [ "$TAG" = "latest" ]; then
        echo "https://api.github.com/repos/$REPO/releases/latest"
    else
        echo "https://api.github.com/repos/$REPO/releases/tags/$TAG"
    fi
}

# @invariant relies only on curl + grep/sed (no jq dependency) so the one-line install works on a bare machine
asset_download_url() {
    local platform="$1" json_url asset_name
    json_url="$(release_json_url)"
    asset_name="edda-bootstrap-${platform}.tar.gz"
    curl -fsSL "$json_url" \
        | grep -o "\"browser_download_url\": *\"[^\"]*${asset_name}\"" \
        | head -n1 \
        | sed -E 's/.*"(https:[^"]+)"/\1/'
}

main() {
    command -v curl >/dev/null 2>&1 || die "curl is required"
    command -v tar >/dev/null 2>&1 || die "tar is required"

    local platform url tmp_dir extracted_dir
    platform="$(detect_platform)"
    log "install.sh: detected platform $platform"

    url="$(asset_download_url "$platform")"
    [ -n "$url" ] || die "could not find a release asset for $platform (tag: $TAG) — see https://github.com/$REPO/releases"

    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    log "install.sh: downloading $url"
    curl -fsSL "$url" -o "$tmp_dir/archive.tar.gz"

    log "install.sh: unpacking"
    tar xzf "$tmp_dir/archive.tar.gz" -C "$tmp_dir"

    extracted_dir="$(find "$tmp_dir" -mindepth 1 -maxdepth 1 -type d | head -n1)"
    [ -n "$extracted_dir" ] || die "unexpected archive layout — no top-level directory found"

    rm -rf "$INSTALL_DIR"
    mkdir -p "$(dirname "$INSTALL_DIR")"
    mv "$extracted_dir" "$INSTALL_DIR"

    log ""
    log "install.sh: installed to $INSTALL_DIR"
    log ""
    add_to_path_hint
}

# @invariant appends at most once — greps the rc file for the exact export line before adding it
add_to_path_hint() {
    local bin_dir rc_file line
    bin_dir="$INSTALL_DIR/bin"
    line="export PATH=\"$bin_dir:\$PATH\""

    case "${SHELL:-}" in
        */zsh) rc_file="$HOME/.zshrc" ;;
        *) rc_file="$HOME/.bashrc" ;;
    esac

    if [ -f "$rc_file" ] && grep -qF "$line" "$rc_file" 2>/dev/null; then
        log "install.sh: $bin_dir is already on PATH via $rc_file"
    else
        printf '\n# Added by edda-bootstrap install.sh\n%s\n' "$line" >> "$rc_file"
        log "install.sh: added $bin_dir to PATH in $rc_file (restart your shell, or run: $line)"
    fi

    log "install.sh: runes vendored at $INSTALL_DIR/runes — use e.g."
    log "  source = \"path+$INSTALL_DIR/runes/lib/<name>\""
    log "install.sh: run 'edda version' after restarting your shell to verify."
}

main "$@"

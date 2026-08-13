#!/bin/sh
set -eu

repository="4piu/utterpipe-pocket-tts"
archive_prefix="utterpipe-pocket-tts"
programs="utterpipe-pocket-tts"
provider_slug="pocket-tts"

usage() {
    echo "usage: install.sh [--version vX.Y.Z] [--install-dir PATH] [--uninstall [--purge]]" >&2
}

version="${VERSION:-}"
install_dir="${INSTALL_DIR:-${XDG_BIN_HOME:-$HOME/.local/bin}}"
uninstall=false
purge=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            version="$2"
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            install_dir="$2"
            shift 2
            ;;
        --uninstall)
            uninstall=true
            shift
            ;;
        --purge)
            purge=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

case "$install_dir" in
    ""|/)
        echo "refusing unsafe install directory: '$install_dir'" >&2
        exit 2
        ;;
esac
if [ "$purge" = true ] && [ "$uninstall" != true ]; then
    echo "--purge requires --uninstall" >&2
    exit 2
fi

purge_provider_assets() {
    [ -n "$provider_slug" ] || return 0
    case "$(uname -s)" in
        Darwin)
            data_root="$HOME/Library/Application Support/UtterPipe/providers/$provider_slug"
            cache_root="$HOME/Library/Caches/UtterPipe/providers/$provider_slug"
            ;;
        Linux)
            data_root="${XDG_DATA_HOME:-$HOME/.local/share}/utterpipe/providers/$provider_slug"
            cache_root="${XDG_CACHE_HOME:-$HOME/.cache}/utterpipe/providers/$provider_slug"
            ;;
        *)
            echo "asset purge is unsupported on this operating system" >&2
            exit 2
            ;;
    esac
    rm -rf -- "$data_root" "$cache_root"
    echo "removed provider assets for $provider_slug (not recoverable)"
}

if [ "$uninstall" = true ]; then
    for program in $programs; do
        rm -f -- "$install_dir/$program"
        echo "removed $install_dir/$program"
    done
    if [ "$purge" = true ]; then
        purge_provider_assets
    fi
    exit 0
fi

case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    Darwin:arm64|Darwin:aarch64)
        target="aarch64-apple-darwin"
        ;;
    Darwin:x86_64|Darwin:amd64)
        target="x86_64-apple-darwin"
        ;;
    *)
        echo "no release artifact is published for $(uname -s) $(uname -m)" >&2
        exit 1
        ;;
esac

if [ -z "$version" ]; then
    latest="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repository/releases/latest")"
    version="${latest##*/}"
fi
if ! printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'; then
    echo "invalid release version: '$version'" >&2
    exit 1
fi

archive="$archive_prefix-$version-$target.tar.gz"
release_url="${RELEASE_BASE_URL:-https://github.com/$repository/releases/download/$version}"
temporary="$(mktemp -d)"
cleanup() {
    rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

curl -fL --retry 3 -o "$temporary/$archive" "$release_url/$archive"
curl -fL --retry 3 -o "$temporary/$archive.sha256" "$release_url/$archive.sha256"
(
    cd "$temporary"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$archive.sha256"
    else
        shasum -a 256 -c "$archive.sha256"
    fi
)
tar -C "$temporary" -xzf "$temporary/$archive"
package_root="$temporary/$archive_prefix-$version-$target"
mkdir -p "$install_dir"
for program in $programs; do
    install -m 755 "$package_root/$program" "$install_dir/$program"
    echo "installed $install_dir/$program"
done

case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *) echo "add $install_dir to PATH before invoking the installed tools" >&2 ;;
esac

installed_executable="$install_dir/utterpipe-pocket-tts"
installed_version="$($installed_executable --version 2>/dev/null || true)"
echo
echo "Pocket TTS provider installation complete."
echo "  Executable: $installed_executable"
[ -z "$installed_version" ] || echo "  Version: $installed_version"
echo "  Checksum: verified"
if ! command -v hf >/dev/null 2>&1; then
    echo
    echo "Optional Hugging Face CLI not found. It is the easiest way to authenticate"
    echo "for the gated quick-start model; the provider itself does not require it."
    echo "  Install guide: https://huggingface.co/docs/huggingface_hub/guides/cli"
    echo "  Alternative: set HF_TOKEN or HF_TOKEN_PATH."
fi
echo
echo "Next steps:"
echo "  1. Accept model access: https://huggingface.co/kyutai/pocket-tts"
echo "  2. Authenticate: hf auth login  (or set HF_TOKEN)"
echo "  3. Prepare the model: $installed_executable models prepare"
echo "  4. Install a voice: $installed_executable voices install"
echo "  5. Check readiness: $installed_executable doctor"

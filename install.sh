#!/bin/sh
# Installs `ziggy`, the tool that sets up Ziggy players:
#   curl -fsSL https://raw.githubusercontent.com/mberg/ziggy/main/install.sh | sh
# Set ZIGGY_INSTALL_DIR to install somewhere other than ~/.local/bin.
set -eu

REPO=mberg/ziggy
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) name=ziggy-macos-arm64 ;;
    Darwin-x86_64) name=ziggy-macos-x86_64 ;;
    *)
        echo "Sorry, there's no ziggy for $(uname -s) $(uname -m) yet." >&2
        exit 1
        ;;
esac

dir=${ZIGGY_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$dir"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${name}…"
curl -fsSL "https://github.com/${REPO}/releases/latest/download/${name}.tar.gz" | tar xz -C "${tmp}"
install -m 755 "$tmp/ziggy" "$dir/ziggy"
echo "Installed $("$dir/ziggy" --version) to $dir/ziggy"

case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        echo
        echo "$dir isn't on your PATH yet. Add this line to ~/.zshrc (or ~/.bashrc), then open a new terminal:"
        echo "  export PATH=\"$dir:\$PATH\""
        ;;
esac
echo
echo "Next: ziggy setup"

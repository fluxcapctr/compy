#!/usr/bin/env bash
# Installs Compy with one line:
#   curl -fsSL https://raw.githubusercontent.com/fluxcapctr/compy/main/get.sh | bash
# On Arch and Omarchy it builds a proper package from the source and installs it with pacman (sudo
# asks once). Anywhere else it takes the latest release tarball and installs it for the current user
# under ~/.local, touching nothing in /usr.
set -euo pipefail
REPO=fluxcapctr/compy
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if command -v pacman >/dev/null 2>&1 && command -v makepkg >/dev/null 2>&1; then
  echo "Arch or Omarchy: building the compy-git package."
  sudo pacman -S --needed --noconfirm git base-devel >/dev/null
  git clone --quiet --depth 1 "https://github.com/$REPO.git" "$work/compy"
  cd "$work/compy/packaging/aur"
  makepkg -si --noconfirm --needed
else
  echo "Fetching the latest Compy release."
  url=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep -o 'https://[^"]*linux-x86_64\.tar\.gz' | head -1)
  [ -n "$url" ] || { echo "No release tarball was found; build from source instead: https://github.com/$REPO" >&2; exit 1; }
  curl -fsSL "$url" | tar xz -C "$work"
  cd "$work"/compy-*
  ./install.sh
  missing=$(ldd "$HOME/.local/bin/compositor" 2>/dev/null | grep "not found" | awk '{print $1}' | tr '\n' ' ')
  if [ -n "$missing" ]; then
    echo
    echo "Compy needs these libraries from your distribution: $missing"
    echo "Debian and Ubuntu: sudo apt install libgtk-4-1 libheif1"
  fi
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "Note: add ~/.local/bin to your PATH to run compy from a terminal." ;; esac
fi

echo
echo "Compy is installed. Open it from your app menu, or run: compy"
echo "Press Ctrl+K inside for the assistant; it walks you through the AI setup."

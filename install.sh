#!/usr/bin/env bash
# Builds the release binary and installs it for the current user: ~/.local/bin/compositor, an app icon, and a
# desktop entry so launchers (Omarchy's included) list it and .psd files open with it. Nothing touches /usr.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
install -Dm755 target/release/compositor "$HOME/.local/bin/compositor"

# The Compy logo, scaled to each icon size (ImageMagick); without it the full-size logo serves every size.
for size in 16 32 48 64 128 256 512; do
  target="$HOME/.local/share/icons/hicolor/${size}x${size}/apps/compy.png"
  mkdir -p "$(dirname "$target")"
  if command -v magick >/dev/null 2>&1; then magick assets/compy-logo.png -resize "${size}x${size}" "$target"; else cp assets/compy-logo.png "$target"; fi
done

install -Dm644 /dev/stdin "$HOME/.local/share/applications/compositor.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Version=1.0
Name=Compy
GenericName=Image Editor
Comment=Compy: layered image editing with masks, adjustments and filters
Exec=compositor %F
Icon=compy
Terminal=false
StartupNotify=true
StartupWMClass=co.ericstevens.compositor
Categories=Graphics;2DGraphics;RasterGraphics;Photography;
Keywords=image;photo;layers;compy;compositor;photoshop;psd;
MimeType=image/vnd.adobe.photoshop;image/png;image/jpeg;image/tiff;image/gif;image/webp;image/bmp;image/heic;image/heif;
DESKTOP

mkdir -p "$HOME/.local/share/compositor/brushes"
for f in assets/brushes/*.abr; do [ -f "$f" ] && install -Dm644 "$f" "$HOME/.local/share/compositor/brushes/$(basename "$f")"; done
for d in assets/brushes/*/; do [ -d "$d" ] && mkdir -p "$HOME/.local/share/compositor/brushes/$(basename "$d")" && cp "$d"/* "$HOME/.local/share/compositor/brushes/$(basename "$d")/"; done

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "installed: $HOME/.local/bin/compositor and ~/.local/share/applications/compositor.desktop"

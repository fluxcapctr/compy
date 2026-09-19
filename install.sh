#!/usr/bin/env bash
# Builds the release binary and installs it for the current user: ~/.local/bin/compositor, an app icon, and a
# desktop entry so launchers (Omarchy's included) list it and .psd files open with it. Nothing touches /usr.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
install -Dm755 target/release/compositor "$HOME/.local/bin/compositor"

icons=reference/Compositor/Assets.xcassets/AppIcon.appiconset
for size in 16 32 64 128 256 512; do
  install -Dm644 "$icons/app-icon-$size.png" "$HOME/.local/share/icons/hicolor/${size}x${size}/apps/compositor.png"
done

install -Dm644 /dev/stdin "$HOME/.local/share/applications/compositor.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Version=1.0
Name=Compositor
GenericName=Image Editor
Comment=Layered image editing with masks, adjustments and filters
Exec=compositor %F
Icon=compositor
Terminal=false
StartupNotify=true
StartupWMClass=co.ericstevens.compositor
Categories=Graphics;2DGraphics;RasterGraphics;Photography;
Keywords=image;photo;layers;compositor;photoshop;psd;
MimeType=image/vnd.adobe.photoshop;image/png;image/jpeg;image/tiff;image/gif;image/webp;image/bmp;
DESKTOP

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "installed: $HOME/.local/bin/compositor and ~/.local/share/applications/compositor.desktop"

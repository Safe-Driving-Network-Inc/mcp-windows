#!/usr/bin/env bash
# Publish a built Kareenos Desktop Agent installer as "latest".
#
# The MSI is built on Windows (the "tray app + MSI" job of
# .github/workflows/kareenos-desktop-agent.yml, artifact
# kareenos-desktop-agent-msi-win32-x64, or `tauri build` on a Windows machine).
# This script takes that MSI and does what the extension's release script does
# for the zip: ONE unversioned file, the sha256, the updater manifest, copied
# into the three trees that serve https://kareenos.com/releases/...
#
#   scripts/build-desktop-agent-release.sh --msi <path/to/file.msi> --version 0.1.0 \
#       [--sig <file.msi.sig>] [--notes "what changed"] [--dry-run]
#
# Trees (all relative to this repo's parent, the gitKareenos folder):
#   mcp-windows/releases/desktop-agent/latest/
#   kareenos_frontend/public/releases/desktop-agent/latest/      (Quasar copies public/ verbatim)
#   expressserver/public/kareenos_com/releases/desktop-agent/latest/   (served by express.static)
# Live after the usual frontend publish and expressserver deploy.
#
# latest.json is the Tauri updater manifest. Without --sig the "signature" field
# is empty and the in-app updater (release builds with --features updater) will
# refuse it; the Downloads page link works either way.
set -euo pipefail

MSI=""; VERSION=""; SIG=""; NOTES=""; DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --msi) MSI="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --sig) SIG="$2"; shift 2 ;;
    --notes) NOTES="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n 2,22p "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$MSI" ] && [ -f "$MSI" ] || { echo "--msi <file> is required and must exist" >&2; exit 2; }
[ -n "$VERSION" ] || { echo "--version <x.y.z> is required (the version in apps/desktop-agent/src-tauri/tauri.conf.json)" >&2; exit 2; }
case "$MSI" in *.msi) ;; *) echo "the installer must be an .msi" >&2; exit 2 ;; esac

HERE="$(cd "$(dirname "$0")/.." && pwd)"
PARENT="$(cd "$HERE/.." && pwd)"
NAME="KareenosDesktopAgent-Setup-x64.msi"
URL="https://kareenos.com/releases/desktop-agent/latest/$NAME"
TREES=(
  "$HERE/releases/desktop-agent/latest"
  "$PARENT/kareenos_frontend/public/releases/desktop-agent/latest"
  "$PARENT/expressserver/public/kareenos_com/releases/desktop-agent/latest"
)

if command -v sha256sum >/dev/null 2>&1; then SHA=$(sha256sum "$MSI" | cut -d' ' -f1); else SHA=$(shasum -a 256 "$MSI" | cut -d' ' -f1); fi
SIGNATURE=""
if [ -n "$SIG" ]; then SIGNATURE="$(tr -d '\n' < "$SIG")"; fi
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
SIZE=$(wc -c < "$MSI" | tr -d ' ')

MANIFEST=$(cat <<EOF
{
  "version": "$VERSION",
  "notes": "${NOTES//\"/\\\"}",
  "pub_date": "$PUB_DATE",
  "platforms": {
    "windows-x86_64": {
      "signature": "$SIGNATURE",
      "url": "$URL"
    }
  }
}
EOF
)

echo "Kareenos Desktop Agent $VERSION"
echo "  msi    : $MSI ($SIZE bytes)"
echo "  sha256 : $SHA"
echo "  signed : $([ -n "$SIGNATURE" ] && echo yes || echo 'no (updater manifest unsigned)')"
for t in "${TREES[@]}"; do
  echo "  -> $t"
  [ "$DRY" -eq 1 ] && continue
  mkdir -p "$t"
  cp "$MSI" "$t/$NAME"
  printf '%s  %s\n' "$SHA" "$NAME" > "$t/$NAME.sha256"
  printf '%s\n' "$MANIFEST" > "$t/latest.json"
  printf '%s\n' "$VERSION" > "$t/VERSION"
done
[ "$DRY" -eq 1 ] && { echo "dry run: nothing written"; exit 0; }
echo "done. Public URL after deploy: $URL"
echo "Next: author the Downloads row in Super Admin › Downloads (download_save) pointing at that URL."

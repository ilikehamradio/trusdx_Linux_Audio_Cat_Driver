#!/usr/bin/env bash
# Build a Gear Lever–compatible AppImage with embedded gh-releases-zsync update info.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ARCH="${ARCH:-x86_64}"
APP_NAME="trusdx_linux_driver"
APP_SHORT="trusdx"
REPO="${GITHUB_REPOSITORY:-ilikehamradio/trusdx_Linux_Audio_Cat_Driver}"
REPO_OWNER="${REPO%%/*}"
REPO_NAME="${REPO#*/}"

if [[ -n "${VERSION:-}" ]]; then
  :
elif [[ -n "${GITHUB_REF_NAME:-}" && "${GITHUB_REF_NAME}" == v* ]]; then
  VERSION="${GITHUB_REF_NAME#v}"
else
  VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
fi

# Keep release asset names short for Gear Lever / downloads.
OUT_NAME="${APP_SHORT}-${ARCH}.AppImage"
# Gear Lever / AppImageUpdate channel:
#   latest     — stable tagged releases (v*)
#   continuous — rolling builds from master
UPDATE_CHANNEL="${UPDATE_CHANNEL:-latest}"
UPDATE_INFORMATION="gh-releases-zsync|${REPO_OWNER}|${REPO_NAME}|${UPDATE_CHANNEL}|${APP_SHORT}-${ARCH}.AppImage.zsync"

DIST_DIR="${ROOT}/dist"
APPDIR="${DIST_DIR}/AppDir"
TOOLS_DIR="${DIST_DIR}/tools"
mkdir -p "$DIST_DIR" "$TOOLS_DIR"

echo "==> Building release binary (version ${VERSION})"
cargo build --release

echo "==> Preparing AppDir"
rm -rf "$APPDIR"
mkdir -p \
  "${APPDIR}/usr/bin" \
  "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/icons/hicolor/256x256/apps"

cp -f "${ROOT}/target/release/${APP_NAME}" "${APPDIR}/usr/bin/${APP_NAME}"
chmod +x "${APPDIR}/usr/bin/${APP_NAME}"
cp -f "${ROOT}/packaging/${APP_NAME}.desktop" "${APPDIR}/usr/share/applications/${APP_NAME}.desktop"
cp -f "${ROOT}/icon.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/${APP_NAME}.png"
cp -f "${ROOT}/icon.png" "${APPDIR}/${APP_NAME}.png"

download() {
  local url="$1"
  local out="$2"
  if [[ -x "$out" ]]; then
    return 0
  fi
  echo "==> Downloading $(basename "$out")"
  curl -fsSL -o "$out" "$url"
  chmod +x "$out"
}

LINUXDEPLOY="${TOOLS_DIR}/linuxdeploy-${ARCH}.AppImage"
PLUGIN="${TOOLS_DIR}/linuxdeploy-plugin-appimage-${ARCH}.AppImage"
download \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${ARCH}.AppImage" \
  "$LINUXDEPLOY"
download \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-${ARCH}.AppImage" \
  "$PLUGIN"

# AppImages in CI often need extract-and-run (no FUSE).
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH
export VERSION
export UPDATE_INFORMATION
export UPD_INFO="${UPDATE_INFORMATION}"
export OUTPUT="${DIST_DIR}/${OUT_NAME}"
export LDAI_OUTPUT="${OUTPUT}"
export LINUXDEPLOY_OUTPUT_VERSION="${VERSION}"

echo "==> Bundling with linuxdeploy"
echo "    UPDATE_INFORMATION=${UPDATE_INFORMATION}"
echo "    OUTPUT=${OUTPUT}"

(
  cd "$DIST_DIR"
  # Plugin must be on PATH for --output appimage
  PATH="${TOOLS_DIR}:${PATH}" \
    "$LINUXDEPLOY" \
      --appdir "$APPDIR" \
      --executable "${APPDIR}/usr/bin/${APP_NAME}" \
      --desktop-file "${APPDIR}/usr/share/applications/${APP_NAME}.desktop" \
      --icon-file "${APPDIR}/usr/share/icons/hicolor/256x256/apps/${APP_NAME}.png" \
      --output appimage
)

# Normalize output name if the plugin chose a different basename.
shopt -s nullglob
built=( "${DIST_DIR}"/*.AppImage )
shopt -u nullglob
if [[ ! -f "${OUTPUT}" ]]; then
  if [[ ${#built[@]} -eq 1 ]]; then
    mv -f "${built[0]}" "${OUTPUT}"
  else
    echo "error: expected AppImage at ${OUTPUT}" >&2
    ls -la "$DIST_DIR" >&2 || true
    exit 1
  fi
fi

# Ensure a matching .zsync exists (plugin usually creates it; fall back to zsyncmake).
ZSYNC_OUT="${OUTPUT}.zsync"
if [[ ! -f "${ZSYNC_OUT}" ]]; then
  if command -v zsyncmake >/dev/null 2>&1; then
    echo "==> Generating ${ZSYNC_OUT##*/} with zsyncmake"
    (
      cd "$DIST_DIR"
      zsyncmake -u "${OUT_NAME}" "${OUT_NAME}"
    )
  else
    echo "error: missing ${ZSYNC_OUT##*/} and zsyncmake is not installed" >&2
    exit 1
  fi
fi

# Re-point zsync URL to the release asset basename when zsyncmake rewrote it.
if [[ -f "${ZSYNC_OUT}" ]]; then
  # Keep the zsync next to the AppImage with the canonical name.
  if [[ ! -f "${DIST_DIR}/${OUT_NAME}.zsync" ]]; then
    mv -f "${ZSYNC_OUT}" "${DIST_DIR}/${OUT_NAME}.zsync"
  fi
fi

echo "==> AppImage ready:"
ls -lh "${DIST_DIR}/${OUT_NAME}" "${DIST_DIR}/${OUT_NAME}.zsync"
echo "    Gear Lever update info: ${UPDATE_INFORMATION}"

#!/bin/sh
# Install Sterna and the inference gateway from a GitHub release.
#
#   curl -fsSL https://harzerheribert.github.io/sterna/install.sh | sh
#   STERNA_VERSION=v0.1.0-pre.2 sh install.sh             # a specific release
#   curl -fsSL https://harzerheribert.github.io/sterna/install.sh | sh -s -- --desktop
#                                                          # the desktop app too
#
# What it does, in order, and nothing else:
#   1. picks the release (the newest, pre-releases included, or
#      $STERNA_VERSION) and this machine's archive;
#   2. refuses the archive unless its SHA-256 matches the release's SHA256SUMS;
#   3. unpacks it into ~/.local/lib/sterna/versions/<tag>/bin -- a fresh
#      directory, never over a binary that may be running -- and points
#      ~/.local/lib/sterna/current at it;
#   4. links sterna and inference-gateway into ~/.local/bin, and removes
#      Pane's old `pane` link and ~/.local/lib/glasshouse install;
#   5. downloads the CLIProxyAPI build the release pins (cliproxyapi.toml),
#      refuses it unless its SHA-256 matches the pin, and hands it to
#      `inference-gateway subscriptions adopt-binary`;
#   6. with --desktop (or STERNA_DESKTOP=1), the desktop app from the same
#      release, verified the same way: into the version directory beside
#      the binaries, then to ~/Applications on macOS (put in place by
#      renames, never over a copy that may be running) or as a menu entry
#      on Linux that opens ~/.local/lib/sterna/current/Sterna.AppImage.
#      Sterna's own updates keep it in step from then on.
# It installs no harness, touches no credential and edits no shell profile.
set -eu

REPO="${STERNA_REPO:-HarzerHeribert/sterna}"
# Test seams: where releases are listed and downloaded from.
API="${STERNA_RELEASES_API:-https://api.github.com/repos/$REPO/releases?per_page=30}"
DOWNLOADS="${STERNA_RELEASE_DOWNLOADS:-https://github.com/$REPO/releases/download}"
BROKER_DOWNLOADS="${STERNA_BROKER_DOWNLOADS:-}"
ROOT="${STERNA_HOME:-$HOME/.local/lib/sterna}"
BIN_DIR="${STERNA_BIN_DIR:-$HOME/.local/bin}"
DESKTOP="${STERNA_DESKTOP:-}"
for arg in "$@"; do
  case "$arg" in
    --desktop) DESKTOP=1 ;;
    *)
      echo "install.sh: unknown option $arg" >&2
      exit 2
      ;;
  esac
done

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "neither curl nor wget is installed"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) die "this installer does not support $(uname -s) $(uname -m); release archives are at https://github.com/$REPO/releases" ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

TAG="${STERNA_VERSION:-}"
if [ -z "$TAG" ]; then
  fetch "$API" "$TMP/releases.json"
  # The list's own order puts pre.9 above pre.10: rank the tags by version,
  # a release above its own pre-releases.
  TAG="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$TMP/releases.json" |
    grep -E '^v[0-9]+\.[0-9]+\.[0-9]+(-pre\.[0-9]+)?$' |
    awk '{ s = substr($0, 2); pre = -1
           if (split(s, a, "-pre.") == 2) { s = a[1]; pre = a[2] }
           split(s, v, ".")
           print v[1], v[2], v[3], (pre < 0), (pre < 0 ? 0 : pre), $0 }' |
    sort -k1,1n -k2,2n -k3,3n -k4,4n -k5,5n | tail -n1 | cut -d' ' -f6)"
  [ -n "$TAG" ] || die "could not read the newest release of $REPO"
fi
VERSION="${TAG#v}"
ARCHIVE="sterna-$VERSION-$TARGET.tar.gz"
BASE="$DOWNLOADS/$TAG"

say "Installing $TAG for $TARGET"
fetch "$BASE/$ARCHIVE" "$TMP/$ARCHIVE"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"
WANT="$(grep " $ARCHIVE\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$WANT" ] || die "$ARCHIVE is not listed in the release's SHA256SUMS"
[ "$(sha256_of "$TMP/$ARCHIVE")" = "$WANT" ] || die "$ARCHIVE does not match its SHA-256; refusing it"

DEST="$ROOT/versions/$TAG"
if [ -x "$DEST/bin/sterna" ]; then
  say "$TAG is already installed at $DEST"
else
  tar xzf "$TMP/$ARCHIVE" -C "$TMP"
  STAGE="$TMP/sterna-$VERSION-$TARGET"
  mkdir -p "$ROOT/versions" "$DEST.partial/bin"
  for b in sterna inference-gateway; do
    [ -f "$STAGE/$b" ] && cp "$STAGE/$b" "$DEST.partial/bin/$b"
  done
  [ -f "$STAGE/cliproxyapi.toml" ] && cp "$STAGE/cliproxyapi.toml" "$DEST.partial/"
  [ -x "$DEST.partial/bin/sterna" ] || die "the archive carried no sterna binary"
  mv "$DEST.partial" "$DEST"
fi
ln -sfn "$DEST" "$ROOT/current"

mkdir -p "$BIN_DIR"
for b in sterna inference-gateway; do
  [ -x "$ROOT/current/bin/$b" ] && ln -sfn "$ROOT/current/bin/$b" "$BIN_DIR/$b"
done

# Sterna was called Pane, which installed into ~/.local/lib/glasshouse and
# linked `pane` next to these. The `pane` link goes; the old install root
# goes once no link in $BIN_DIR points into it any more.
OLD_ROOT="$HOME/.local/lib/glasshouse"
if [ -L "$BIN_DIR/pane" ]; then
  case "$(readlink "$BIN_DIR/pane")" in
    "$OLD_ROOT"/*)
      rm -f "$BIN_DIR/pane"
      say "Removed the old pane command; sterna replaces it."
      ;;
  esac
fi
if [ -d "$OLD_ROOT" ]; then
  LINKED=""
  for link in "$BIN_DIR"/*; do
    [ -L "$link" ] || continue
    case "$(readlink "$link")" in "$OLD_ROOT"/*) LINKED="$link" ;; esac
  done
  if [ -z "$LINKED" ]; then
    rm -rf "$OLD_ROOT"
    say "Removed Pane's old install at $OLD_ROOT; your settings and sessions move on sterna's first start."
  fi
fi

# The subscription broker the release was built with.
PIN="$DEST/cliproxyapi.toml"
if [ -f "$PIN" ]; then
  BROKER_REPO="$(sed -n 's/^repository = "\(.*\)"/\1/p' "$PIN")"
  BROKER_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$PIN")"
  LINE="$(grep "^$TARGET = " "$PIN" || true)"
  NAME="$(printf '%s' "$LINE" | sed -n 's/.*name = "\([^"]*\)".*/\1/p')"
  SUM="$(printf '%s' "$LINE" | sed -n 's/.*sha256 = "\([^"]*\)".*/\1/p')"
  if [ -n "$NAME" ] && [ -n "$SUM" ] && [ "$(cat "$ROOT/broker-version" 2>/dev/null)" != "$BROKER_VERSION" ]; then
    fetch "${BROKER_DOWNLOADS:-https://github.com/$BROKER_REPO/releases/download}/v$BROKER_VERSION/$NAME" "$TMP/$NAME"
    [ "$(sha256_of "$TMP/$NAME")" = "$SUM" ] || die "$NAME does not match the SHA-256 the release pins; refusing it"
    mkdir -p "$TMP/broker"
    tar xzf "$TMP/$NAME" -C "$TMP/broker"
    "$DEST/bin/inference-gateway" subscriptions adopt-binary "$TMP/broker/cli-proxy-api" >/dev/null
    printf '%s' "$BROKER_VERSION" > "$ROOT/broker-version"
    say "Subscription broker: CLIProxyAPI $BROKER_VERSION"
  fi
fi

# The desktop app, from the same release.
if [ -n "$DESKTOP" ]; then
  case "$TARGET" in
    aarch64-apple-darwin) APP_ENTRIES="Sterna.app" ;;
    x86_64-unknown-linux-gnu) APP_ENTRIES="Sterna.AppImage sterna.png" ;;
    *) die "the desktop app is built for macOS on Apple silicon and Linux on x86_64; sterna itself is installed" ;;
  esac
  APP_ARCHIVE="sterna-desktop-$VERSION-$TARGET.tar.gz"
  APP_WANT="$(grep " $APP_ARCHIVE\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
  [ -n "$APP_WANT" ] || die "$TAG carries no desktop app for $TARGET; sterna itself is installed"
  APP_FIRST="${APP_ENTRIES%% *}"
  if [ ! -e "$DEST/$APP_FIRST" ]; then
    fetch "$BASE/$APP_ARCHIVE" "$TMP/$APP_ARCHIVE"
    [ "$(sha256_of "$TMP/$APP_ARCHIVE")" = "$APP_WANT" ] || die "$APP_ARCHIVE does not match its SHA-256; refusing it"
    mkdir -p "$TMP/desktop"
    tar xzf "$TMP/$APP_ARCHIVE" -C "$TMP/desktop"
    for entry in $APP_ENTRIES; do
      [ -e "$TMP/desktop/$entry" ] || die "$APP_ARCHIVE carried no $entry"
      mv "$TMP/desktop/$entry" "$DEST/$entry"
    done
  fi
  : > "$ROOT/desktop"
  case "$TARGET" in
    *-apple-darwin)
      APPS="${STERNA_APPLICATIONS:-$HOME/Applications}"
      mkdir -p "$APPS"
      rm -rf "$APPS/.Sterna.app.$$" "$APPS/.Sterna.app.old.$$"
      cp -R "$DEST/Sterna.app" "$APPS/.Sterna.app.$$"
      if [ -e "$APPS/Sterna.app" ]; then mv "$APPS/Sterna.app" "$APPS/.Sterna.app.old.$$"; fi
      mv "$APPS/.Sterna.app.$$" "$APPS/Sterna.app"
      rm -rf "$APPS/.Sterna.app.old.$$"
      # The marker names the copy, so the app's own sessions find this
      # install and keep the app up to date.
      printf '%s\n' "$APPS/Sterna.app" > "$ROOT/desktop"
      say "Desktop app: $APPS/Sterna.app"
      ;;
    *)
      chmod 755 "$DEST/Sterna.AppImage"
      LAUNCHERS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
      mkdir -p "$LAUNCHERS"
      printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=Sterna" \
        "Comment=Watch and answer your coding sessions" \
        "Exec=$ROOT/current/Sterna.AppImage %U" "Icon=$ROOT/current/sterna.png" \
        "Categories=Development;" "Terminal=false" > "$LAUNCHERS/.sterna.desktop.$$"
      mv "$LAUNCHERS/.sterna.desktop.$$" "$LAUNCHERS/sterna.desktop"
      say "Desktop app: Sterna in your applications menu ($ROOT/current/Sterna.AppImage)"
      ;;
  esac
fi

say "Installed $TAG. Sterna updates itself from here on; run \`sterna\` to start, \`sterna doctor\` to check."
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "Add $BIN_DIR to your PATH to run sterna from any shell." ;;
esac

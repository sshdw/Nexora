#!/usr/bin/env bash
#
# deploy.sh — Atomic static-site deployment for Nexora website.
#
# Usage: ./deploy.sh <build-dir> <releases-root> <current-symlink> <domain>
#
#   build-dir      Path to freshly built `out/` directory (from `next build`)
#   releases-root  Parent directory for versioned releases (e.g. /var/www/nexora/releases)
#   current-symlink Path to the `current` symlink (e.g. /var/www/nexora/current)
#   domain         Hostname for health check (e.g. prismdev.duckdns.org)
#
# Behaviour (TZ §57):
#   1. Copy build into a new timestamped release directory under releases-root
#   2. Run health check against the new release via a temporary Caddy vhost or curl
#   3. Atomically switch `current` symlink to the new release
#   4. On failed health check: keep previous deployment, remove failed release
#   5. Retain previous successful releases; prune old ones beyond a limit
#
# Exit codes:
#   0  success
#   1  usage error
#   2  build directory missing
#   3  health check failed (rollback performed)
#   4  symlink switch failed

set -euo pipefail

KEEP_RELEASES=5

usage() {
    echo "Usage: $0 <build-dir> <releases-root> <current-symlink> <domain>" >&2
    exit 1
}

if [ "$#" -ne 4 ]; then
    usage
fi

BUILD_DIR="$1"
RELEASES_ROOT="$2"
CURRENT_LINK="$3"
DOMAIN="$4"

if [ ! -d "$BUILD_DIR" ]; then
    echo "ERROR: build directory not found: $BUILD_DIR" >&2
    exit 2
fi

mkdir -p "$RELEASES_ROOT"

RELEASE_NAME="$(date +%Y%m%d%H%M%S)"
RELEASE_DIR="$RELEASES_ROOT/$RELEASE_NAME"

echo "[deploy] Copying build to $RELEASE_DIR ..."
cp -r "$BUILD_DIR" "$RELEASE_DIR"

# Health check: curl the new release's index.html directly (file-based, no server needed)
# We verify the key routes exist as static files.
echo "[deploy] Health check: verifying static files in $RELEASE_DIR ..."
# Static export with trailingSlash: routes are directories with index.html.
# Root route is index.html at the release root.
routes="security docs download changelog privacy"
for route in $routes; do
    if [ ! -f "$RELEASE_DIR/$route/index.html" ]; then
        echo "ERROR: health check failed — missing route file for: /$route" >&2
        echo "[deploy] Removing failed release $RELEASE_DIR ..."
        rm -rf "$RELEASE_DIR"
        exit 3
    fi
done
if [ ! -f "$RELEASE_DIR/index.html" ]; then
    echo "ERROR: health check failed — missing route file for: /" >&2
    echo "[deploy] Removing failed release $RELEASE_DIR ..."
    rm -rf "$RELEASE_DIR"
    exit 3
fi

echo "[deploy] Health check passed — all routes present."

# Atomic symlink switch
NEW_LINK_TMP="$CURRENT_LINK.tmp.$$"
ln -snf "$RELEASE_DIR" "$NEW_LINK_TMP"
if ! mv -Tf "$NEW_LINK_TMP" "$CURRENT_LINK"; then
    echo "ERROR: failed to switch current symlink" >&2
    rm -rf "$NEW_LINK_TMP" 2>/dev/null || true
    rm -rf "$RELEASE_DIR" 2>/dev/null || true
    exit 4
fi

echo "[deploy] Switched $CURRENT_LINK -> $RELEASE_DIR"

# Prune old releases (keep KEEP_RELEASES)
echo "[deploy] Pruning old releases (keeping last $KEEP_RELEASES) ..."
# shellcheck disable=SC2012
ls -1dt "$RELEASES_ROOT"/* | tail -n +$((KEEP_RELEASES + 1)) | xargs -r rm -rf -- 2>/dev/null || true

echo "[deploy] Deployment complete. Current: $CURRENT_LINK -> $(readlink "$CURRENT_LINK")"

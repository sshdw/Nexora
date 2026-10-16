#!/usr/bin/env bash
# capture-tauri.sh — reproducible REAL-Tauri (WebKitGTK) screenshot capture for Nexora.
#
# WHAT THIS IS FOR
#   Nexora visual proof must come from the real Tauri/WebKitGTK window running
#   the real SQLite backend — never from headless Chromium against `?mock`.
#   This script launches ./src-tauri/target/debug/nexora on a private Xvfb and
#   captures the window only once the WebView has actually painted.
#
# WHY IT POLLS FOR *SUSTAINED* PAINT
#   A first-paint check is a trap. Before the a1de895 locale fix, the Tauri
#   WebView on this box reliably painted ONE full frame roughly 1.0s-1.5s
#   after launch and then the content layer went permanently black (an app
#   bug — `formatRelativeTime` throwing on `navigator.language === "c"` —
#   not the host path). A script that accepts the first frame above a colour
#   threshold will happily "succeed" with a screenshot of a window that is dead
#   two hundred milliseconds later. This script requires N CONSECUTIVE samples
#   above the threshold before it calls the capture good, and exits non-zero
#   otherwise. See "KNOWN FAILURE MODE" below.
#
# ENVIRONMENT (measured on this box, arm64 / Oracle A1 / Ubuntu 24.04)
#   Xvfb     :77 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER \
#                 +render -noreset
#   WM       openbox (optional but recommended; gives a sane focus/stack order)
#   Comp.    xcompmgr (optional; changes when the first frame appears)
#   WebKit   WEBKIT_DISABLE_COMPOSITING_MODE=1
#     ^ The ONLY env that reliably makes the Tauri window produce any frame at
#       all. Every other combination tried (see the table in the PR body) paints
#       nothing whatsoever.
#
#   DELIBERATELY *NOT* SET — these were all tried and all make things worse:
#     LIBGL_ALWAYS_SOFTWARE=1  + GALLIUM_DRIVER=swrast
#         ACTUALLY BREAKS GL on this box. `glxinfo -B` fails with
#         "glx: failed to create drisw screen" / BadValue on X_GLXCreateNewContext.
#         Every prior blank capture that set these was fighting a self-inflicted
#         wound, not the real bug.
#     GALLIUM_DRIVER=llvmpipe   same failure.
#     WEBKIT_DISABLE_DMABUF_RENDERER=1 together with LIBGL_ALWAYS_SOFTWARE=1
#         makes the app abort at startup (process dies before the window maps).
#
#   NOT REQUIRED (the warning is cosmetic — EGL silently falls back to llvmpipe):
#     /dev/dri/renderD128 + /dev/dri/card0 access. The recurring
#     "libEGL warning: failed to open /dev/dri/renderD128: Permission denied"
#     is a DRI3 fallback notice, NOT the fault. Adding the user to the video and
#     render groups silences it and changes nothing about painting. Also note
#     `chmod g+rw` on those nodes is a silent no-op here (rc=0, mode unchanged),
#     so group membership is the only lever if you want the warning gone.
#
#   PREREQUISITES (apt): xvfb, openbox, x11-apps (import), imagemagick,
#                        xdotool, xcompmgr (optional).
#     xcompmgr was installed on this box for the compositor experiments.
#
# KNOWN FAILURE MODE (RESOLVED 2026-10-16 in commit a1de895 — kept as history)
#   The symptom below ("paints one frame, then the content layer stops
#   forever") was NEVER the Tauri host path: it was `src/lib/format.ts`
#   throwing `RangeError` on `navigator.language === "c"` once IPC rows
#   resolved, unmounting the whole tree. The minimal-WebKitGTK exoneration
#   held paint only because it never resolved rows. After the fix the same
#   recipe sustains paint indefinitely (947 colours across 19 consecutive
#   samples) — see docs/visual-proof/230-p1-real/ frames 02-10. The
#   sustained-paint polling stays because first-paint checking is still a
#   trap in general.
#   Original (pre-fix) symptom, past tense: with
#   WEBKIT_DISABLE_COMPOSITING_MODE=1 the Tauri window painted one full
#   frame at ~1.0s-1.5s after launch, then the WebView content layer
#   stopped producing frames: window stayed IsViewable, the app process
#   and WebKitWebProcess stayed alive with flat RSS, no page reload
#   occurred, and forced resize / unmap+remap / windowraise / mouse and
#   keyboard input all failed to revive it.
#
# USAGE
#   docs/visual-proof/capture-tauri.sh <output.png> [--display :77] [--env "K=V ..."]
#                                        [--samples 30] [--interval 0.5]
#                                        [--min-colours 200] [--sustain 5]
#                                        [--settle 8] [--geometry WxH+X+Y]
#
# EXIT CODES
#   0  sustained paint observed and <output.png> written
#   2  bad usage / missing prerequisite
#   3  the window never sustained paint (the historical failure mode,
#      resolved by the a1de895 locale fix) — nothing written
#   4  the app process died before painting
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${NEXORA_BIN:-$REPO_ROOT/src-tauri/target/debug/nexora}"

OUT=""; DISPLAY_NUM=":77"; EXTRA_ENV=""
SAMPLES=30; INTERVAL=0.5; MIN_COLOURS=200; SUSTAIN=5; SETTLE=8; GEOM=""
USE_WM=1; USE_COMP=0; TITLE='^Nexora$'; BIN_ARG=""

die() { printf 'capture-tauri: %s\n' "$1" >&2; exit "${2:-2}"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --display)      DISPLAY_NUM="$2"; shift 2 ;;
    --env)          EXTRA_ENV="$2"; shift 2 ;;
    --title)        TITLE="$2"; shift 2 ;;
    --bin-arg)      BIN_ARG="$2"; shift 2 ;;
    --samples)      SAMPLES="$2"; shift 2 ;;
    --interval)     INTERVAL="$2"; shift 2 ;;
    --min-colours)  MIN_COLOURS="$2"; shift 2 ;;
    --sustain)      SUSTAIN="$2"; shift 2 ;;
    --settle)       SETTLE="$2"; shift 2 ;;
    --geometry)     GEOM="$2"; shift 2 ;;
    --no-wm)        USE_WM=0; shift ;;
    --compositor)   USE_COMP=1; shift ;;
    -h|--help)      sed -n '2,60p' "$0"; exit 0 ;;
    -*)             die "unknown option $1" ;;
    *)              [ -z "$OUT" ] || die "unexpected extra argument $1"; OUT="$1"; shift ;;
  esac
done

[ -n "$OUT" ]  || die "usage: capture-tauri.sh <output.png> [options]"
[ -x "$BIN" ] || die "binary not found or not executable: $BIN (build it first: cargo build)"
for tool in Xvfb import convert xdotool; do
  command -v "$tool" >/dev/null 2>&1 || die "missing prerequisite: $tool"
done

mkdir -p "$(dirname "$OUT")"
TMP="$(mktemp -d)"
cleanup() {
  [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null || true
  [ -n "${WM_PID:-}" ]    && kill "$WM_PID" 2>/dev/null || true
  [ -n "${COMP_PID:-}" ]  && kill "$COMP_PID" 2>/dev/null || true
  [ -n "${APP_PID:-}" ]   && kill "$APP_PID" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

# --- private Xvfb ------------------------------------------------------------
# A private display number matters: this box has been shared by several agents,
# and a long-lived shared :99 being reset underneath a capture is indistinguishable
# from "the WebView never painted".
DISPLAY_NUM="${DISPLAY_NUM#:}"
rm -f "/tmp/.X${DISPLAY_NUM}-lock"
Xvfb ":$DISPLAY_NUM" -screen 0 1600x1000x24 -ac \
     +extension GLX +extension RENDER +render -noreset \
     >"$TMP/xvfb.log" 2>&1 &
XVFB_PID=$!
export DISPLAY=":$DISPLAY_NUM"
for _ in $(seq 1 50); do
  xdpyinfo >/dev/null 2>&1 && break
  sleep 0.2
done
xdpyinfo >/dev/null 2>&1 || die "Xvfb :$DISPLAY_NUM did not come up (see $TMP/xvfb.log)" 2

if [ "$USE_WM" -eq 1 ] && command -v openbox >/dev/null 2>&1; then
  openbox >"$TMP/openbox.log" 2>&1 & WM_PID=$!
  sleep 1
fi
if [ "$USE_COMP" -eq 1 ] && command -v xcompmgr >/dev/null 2>&1; then
  xcompmgr -n 24 >"$TMP/xcompmgr.log" 2>&1 & COMP_PID=$!
  sleep 1
fi

# --- launch the real binary --------------------------------------------------
export WEBKIT_DISABLE_COMPOSITING_MODE=1
if [ -n "$BIN_ARG" ]; then
  # shellcheck disable=SC2086
  setsid env $EXTRA_ENV "$BIN" $BIN_ARG >"$TMP/app.log" 2>&1 &
else
  # shellcheck disable=SC2086
  setsid env $EXTRA_ENV "$BIN" >"$TMP/app.log" 2>&1 &
fi
APP_PID=$!

# --- locate the real window (skip the 10x10 GTK group-leader window) ---------
WIN=""
for _ in $(seq 1 40); do
  WIN="$(xdotool search --name "$TITLE" 2>/dev/null | while read -r w; do
           g="$(xdotool getwindowgeometry "$w" 2>/dev/null | grep Geometry: | grep -oE '[0-9]+x[0-9]+' || true)"
           if [ -n "$g" ] && [ "$g" != "10x10" ]; then echo "$w"; break; fi
           :
         done || true)"
  if [ -n "$WIN" ]; then break; fi
  sleep 0.25
done
[ -n "$WIN" ] || die "no Nexora window appeared" 4

if [ -z "$GEOM" ]; then
  GEOM="$(xdotool getwindowgeometry --shell "$WIN" \
          | awk -F= '/^X=/{x=$2} /^Y=/{y=$2} /^WIDTH=/{w=$2} /^HEIGHT=/{h=$2} \
                     END{printf "%sx%s+%s+%s", w, h, x, y}')"
fi
printf 'capture-tauri: window 0x%x  crop %s  display %s\n' "$WIN" "$GEOM" "$DISPLAY"

sleep "$SETTLE"
# --- poll for SUSTAINED paint -----------------------------------------------
run=0; max=0
for i in $(seq 1 "$SAMPLES"); do
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    printf 'capture-tauri: app died before painting; last log lines:\n' >&2
    grep -viE 'libEGL warning|^$' "$TMP/app.log" | tail -15 >&2 || true
    exit 4
  fi
  import -window root "$TMP/probe.png" 2>/dev/null || true
  n="$(convert "$TMP/probe.png" -crop "$GEOM" +repage -format '%k' info: 2>/dev/null || echo 0)"
  if [ "${n:-0}" -gt "$max" ]; then max=$n; fi
  if [ "${n:-0}" -ge "$MIN_COLOURS" ]; then
    run=$((run + 1)); printf '  sample %2d/%s colours=%-6s run=%s/%s\n' "$i" "$SAMPLES" "$n" "$run" "$SUSTAIN"
  else
    if [ "$run" -gt 0 ]; then printf '  sample %2d/%s colours=%-6s run RESET\n' "$i" "$SAMPLES" "$n"; fi
    run=0
  fi
  if [ "$run" -ge "$SUSTAIN" ]; then
    import -window root "$OUT" 2>/dev/null || true
    convert "$OUT" -crop "$GEOM" +repage "$OUT"
    printf 'capture-tauri: OK sustained paint (%s consecutive samples, last=%s colours) -> %s\n' \
      "$SUSTAIN" "$n" "$OUT"
    exit 0
  fi
  sleep "$INTERVAL"
done

printf 'capture-tauri: FAILED — no sustained paint in %s samples (max %s colours, threshold %s).\n' \
  "$SAMPLES" "$max" "$MIN_COLOURS" >&2
printf 'capture-tauri: this is the historical no-sustained-paint mode (issue #245; resolved for the P1 list by the a1de895 locale fix); nothing written.\n' >&2
exit 3

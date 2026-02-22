#!/usr/bin/env bash
# cleanup.sh — Revoke macOS TCC permissions granted for computer-use sessions.
#
# WARNING: tccutil resets ALL grants for the specified bundle ID, not just
# the ones granted for UnixAgent sessions. If your terminal app has other
# Accessibility or Screen Recording grants, they will be revoked too.
#
# Usage:
#   ./cleanup.sh              # Reset permissions for Terminal.app
#   ./cleanup.sh com.iTerm2   # Reset permissions for iTerm2

set -euo pipefail

BUNDLE_ID="${1:-com.apple.Terminal}"

echo "Revoking TCC permissions for: $BUNDLE_ID"
echo ""
echo "WARNING: This resets ALL Accessibility and Screen Recording grants"
echo "for this app, not just ones used by UnixAgent."
echo ""
read -rp "Continue? [y/N] " yn
case "$yn" in
    [Yy]*) ;;
    *) echo "Aborted."; exit 0 ;;
esac

echo ""
echo "Resetting Accessibility..."
tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null && echo "  Done" || echo "  Failed (may require sudo)"

echo "Resetting Screen Recording..."
tccutil reset ScreenCapture "$BUNDLE_ID" 2>/dev/null && echo "  Done" || echo "  Failed (may require sudo)"

echo ""
echo "Permissions revoked. You will be prompted again next time."
echo ""
echo "Common bundle IDs:"
echo "  com.apple.Terminal       Terminal.app"
echo "  com.googlecode.iterm2    iTerm2"
echo "  com.github.wez.wezterm   WezTerm"
echo "  io.alacritty             Alacritty"

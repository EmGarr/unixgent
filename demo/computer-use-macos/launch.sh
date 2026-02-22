#!/usr/bin/env bash
# launch.sh — Start a macOS computer-use session with UnixAgent.
#
# Usage:
#   ./launch.sh "Take a screenshot and describe what you see"
#   ./launch.sh "Open Safari and navigate to example.com"
#
# Prerequisites:
#   - cliclick installed (brew install cliclick)
#   - Accessibility permission granted (System Settings > Privacy > Accessibility)
#   - Screen Recording permission granted (System Settings > Privacy > Screen Recording)
#   - ANTHROPIC_API_KEY set or configured in ~/.config/unixagent/config.toml

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# --- Check prompt ---
if [ $# -eq 0 ]; then
    echo "Usage: $0 \"<instruction>\""
    echo ""
    echo "Examples:"
    echo "  $0 \"Take a screenshot and describe what you see\""
    echo "  $0 \"Open Safari and go to example.com\""
    exit 1
fi

PROMPT="$*"

# --- Check cliclick ---
if ! command -v cliclick &>/dev/null; then
    echo "error: cliclick not found"
    echo "install: brew install cliclick"
    exit 1
fi

# --- Test Accessibility permission ---
echo "Checking Accessibility permission..."
if ! cliclick p &>/dev/null; then
    echo "error: Accessibility permission not granted for this terminal app"
    echo ""
    echo "Fix: System Settings > Privacy & Security > Accessibility"
    echo "     Add and enable your terminal app (Terminal.app, iTerm2, etc.)"
    exit 1
fi
echo "  OK"

# --- Test Screen Recording permission ---
echo "Checking Screen Recording permission..."
TMPSHOT="/tmp/.ua-screencapture-test.png"
if ! screencapture -x "$TMPSHOT" 2>/dev/null; then
    echo "error: screencapture failed"
    exit 1
fi
# Check if the screenshot is not just a blank/tiny file (permission denied gives 0-byte or tiny)
FILESIZE=$(stat -f%z "$TMPSHOT" 2>/dev/null || echo "0")
rm -f "$TMPSHOT"
if [ "$FILESIZE" -lt 1000 ]; then
    echo "warning: screenshot seems empty — Screen Recording permission may not be granted"
    echo ""
    echo "Fix: System Settings > Privacy & Security > Screen Recording"
    echo "     Add and enable your terminal app"
    echo ""
    read -rp "Continue anyway? [y/N] " yn
    case "$yn" in
        [Yy]*) ;;
        *) exit 1 ;;
    esac
fi
echo "  OK"

# --- Resolve API key ---
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    # Try macOS Keychain
    KEY=$(security find-generic-password -s "anthropic_api_key" -w 2>/dev/null || true)
    if [ -n "$KEY" ]; then
        export ANTHROPIC_API_KEY="$KEY"
    else
        echo "error: ANTHROPIC_API_KEY not set and not found in Keychain"
        echo ""
        echo "Set via env:     export ANTHROPIC_API_KEY='sk-ant-...'"
        echo "Set via Keychain: security add-generic-password -s anthropic_api_key -a \$USER -w 'sk-ant-...'"
        exit 1
    fi
fi

# --- Launch ---
echo ""
echo "Starting computer-use session..."
echo "  Prompt: $PROMPT"
echo "  Judge: Block mode (all non-read-only commands reviewed)"
echo "  Sandbox: Seatbelt (filesystem isolation)"
echo ""

export UNIXAGENT_COMPUTER_USE=macos

exec cargo run -p ua-core -- \
    -p "$PROMPT" \
    --system-prompt-file "$SCRIPT_DIR/system-prompt.md"

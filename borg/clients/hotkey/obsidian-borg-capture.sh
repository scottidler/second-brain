#!/usr/bin/env bash
# obsidian-borg-capture.sh — capture clipboard URL to obsidian-borg
#
# Bind to a global hotkey:
#   GNOME:  Settings > Keyboard > Custom Shortcuts
#   KDE:    System Settings > Shortcuts > Custom Shortcuts
#   i3:     bindsym $mod+b exec ~/.local/bin/obsidian-borg-capture.sh
#   sway:   bindsym $mod+b exec ~/.local/bin/obsidian-borg-capture.sh
#   macOS:  Automator Quick Action + System Settings > Keyboard > Shortcuts
#           or Hammerspoon: hs.hotkey.bind({"alt","shift"}, "b", function() ... end)

set -euo pipefail

# Cross-platform notification
notify_msg() {
    local msg="$1"
    local urgency="${2:-low}"
    case "$(uname)" in
        Darwin) osascript -e "display notification \"$msg\" with title \"obsidian-borg\"" ;;
        Linux)  notify-send "obsidian-borg" "$msg" --urgency="$urgency" 2>/dev/null || echo "$msg" ;;
    esac
}

# Cross-platform clipboard read
case "$(uname)" in
    Darwin) URL="$(pbpaste)" ;;
    Linux)
        if command -v wl-paste &>/dev/null && [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
            URL="$(wl-paste 2>/dev/null)"
        else
            URL="$(xclip -selection clipboard -o 2>/dev/null)"
        fi
        ;;
    *) echo "Unsupported OS" >&2; exit 1 ;;
esac

if [[ -z "$URL" || ! "$URL" =~ ^https?:// ]]; then
    notify_msg "No URL found on clipboard" "normal"
    exit 1
fi

RESULT=$(obsidian-borg ingest "$URL" 2>&1)
EXIT_CODE=$?

if [[ $EXIT_CODE -eq 0 ]]; then
    notify_msg "$RESULT"
else
    notify_msg "Failed: $RESULT" "critical"
fi

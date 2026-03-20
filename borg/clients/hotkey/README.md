# obsidian-borg Hotkey Script

System-wide URL capture via clipboard and global hotkey.

## Workflow

1. Copy a URL in your browser (Ctrl+L, Ctrl+C)
2. Press your configured global hotkey
3. A desktop notification confirms capture

## Requirements

| Platform | Clipboard | Notifications |
|----------|-----------|---------------|
| macOS | `pbpaste` (built-in) | `osascript` (built-in) |
| Linux (X11) | `xclip` | `notify-send` (libnotify) |
| Linux (Wayland) | `wl-paste` (wl-clipboard) | `notify-send` (libnotify) |

`obsidian-borg` must be on `$PATH`.

## Setup

Copy the script somewhere on your PATH:

```bash
cp obsidian-borg-capture.sh ~/.local/bin/
```

Then bind it to a hotkey:

- **GNOME:** Settings > Keyboard > Custom Shortcuts > Add `~/.local/bin/obsidian-borg-capture.sh`
- **KDE:** System Settings > Shortcuts > Custom Shortcuts
- **i3/sway:** `bindsym $mod+b exec ~/.local/bin/obsidian-borg-capture.sh`
- **macOS:** Automator Quick Action + System Settings > Keyboard > Shortcuts, or Hammerspoon

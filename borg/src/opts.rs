#[derive(Debug, Clone)]
pub struct HotkeyOpts {
    /// Install the keyboard shortcut
    pub install: bool,

    /// Uninstall the keyboard shortcut
    pub uninstall: bool,

    /// Key binding in GNOME format (default: <Ctrl><Shift>b)
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct DaemonOpts {
    /// Install system service (idempotent - safe to run repeatedly)
    pub install: bool,

    /// Uninstall system service
    pub uninstall: bool,

    /// Reinstall system service (full teardown then install)
    pub reinstall: bool,

    /// Start daemon (used by systemd ExecStart)
    pub start: bool,

    /// Stop daemon
    pub stop: bool,

    /// Restart daemon
    pub restart: bool,

    /// Show daemon status
    pub status: bool,
}

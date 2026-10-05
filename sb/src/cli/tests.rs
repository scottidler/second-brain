use super::*;

fn cmd(args: &[&str]) -> Cmd {
    let mut argv = vec!["sb"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).expect("parses").cmd
}

#[test]
fn the_foreground_daemons_keep_their_own_signal_handling() {
    assert!(cmd(&["borg", "daemon", "--start"]).is_long_running_daemon());
    assert!(cmd(&["cortex", "daemon", "--start"]).is_long_running_daemon());
    assert!(
        cmd(&["cortex", "daemon"]).is_long_running_daemon(),
        "a bare `cortex daemon` runs in the foreground"
    );
}

#[test]
fn every_other_command_gets_the_interrupt_handler() {
    for args in [
        &["borg", "daemon", "--status"][..],
        &["borg", "daemon"],
        &["borg", "queue"],
        &["cortex", "daemon", "--install"],
        &["cortex", "daemon", "--status"],
        &["cortex", "daemon", "--stop"],
        &["cortex", "daemon", "--uninstall"],
        &["oracle", "serve"],
        &["doctor"],
        &["status"],
    ] {
        assert!(!cmd(args).is_long_running_daemon(), "{args:?}");
    }
}

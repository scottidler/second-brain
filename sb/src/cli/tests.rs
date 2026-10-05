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

/// Parse a command line the way the OS will run it: through sb's own clap,
/// not a string match, so a subcommand that does not exist fails here.
fn parse_with_sb(exe_then_args: &[&str]) -> Cmd {
    assert_eq!(exe_then_args[0], "/opt/bin/sb", "fixture exe leads the command");
    cmd(&exe_then_args[1..])
}

#[test]
fn the_hotkey_command_parses_as_a_borg_clipboard_ingest() {
    let command = ::borg::service::hotkey_command(std::path::Path::new("/opt/bin/sb"));
    let words: Vec<&str> = command.split_whitespace().collect();
    match parse_with_sb(&words) {
        Cmd::Borg(c) => match c.command {
            Some(borg::Command::Ingest { clipboard, url, .. }) => {
                assert!(clipboard, "the hotkey must read the clipboard");
                assert!(url.is_none());
            }
            _ => panic!("hotkey command did not parse as `borg ingest`: {command}"),
        },
        _ => panic!("hotkey command did not parse as a borg command: {command}"),
    }
}

#[test]
fn the_launchd_plist_runs_a_command_that_starts_the_borg_daemon() {
    let plist = ::borg::service::render_launchd_plist("/opt/bin/sb");
    let program_arguments = plist
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .and_then(|rest| rest.split("</array>").next())
        .expect("plist has ProgramArguments");
    let words: Vec<&str> = program_arguments
        .split("<string>")
        .skip(1)
        .map(|s| s.split("</string>").next().expect("closed string"))
        .collect();
    assert!(cmd_is_borg_daemon_start(parse_with_sb(&words)), "plist args: {words:?}");
}

fn cmd_is_borg_daemon_start(parsed: Cmd) -> bool {
    parsed.is_long_running_daemon() && matches!(parsed, Cmd::Borg(_))
}

#[test]
fn the_old_hotkey_and_plist_forms_do_not_parse() {
    for args in [&["ingest", "--clipboard"][..], &["daemon", "--start"]] {
        let mut argv = vec!["sb"];
        argv.extend_from_slice(args);
        assert!(Cli::try_parse_from(argv).is_err(), "{args:?} is not an sb command");
    }
}

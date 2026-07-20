use super::*;
use crate::config::Config;
use std::path::PathBuf;

fn cfg(schedule: &str) -> Config {
    let mut c = Config::default();
    c.harvest.schedule = schedule.to_string();
    c
}

#[test]
fn service_uses_absolute_binary_and_explicit_path() {
    // The stripped-timer-PATH criterion: an absolute ExecStart binary PLUS an
    // explicit `Environment="PATH=..."` means the unit resolves even with an
    // empty inherited PATH (a systemd timer's environment).
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &cfg("daily"));
    assert!(
        service.contains("ExecStart=/home/tester/.cargo/bin/sb borg harvest"),
        "ExecStart must use the absolute binary path, not a bare `sb`:\n{service}"
    );
    assert!(
        service.contains("Environment=\"PATH="),
        "service must set an explicit PATH so it runs with an empty inherited env"
    );
    assert!(service.contains("Type=oneshot"), "harvest is a batch job, not a daemon");
}

#[test]
fn timer_bakes_only_oncalendar_from_config() {
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (_service, timer) = render_units(&home, &binary, &cfg("*-*-* 04:30:00"));
    assert!(
        timer.contains("OnCalendar=*-*-* 04:30:00"),
        "OnCalendar is rendered from harvest.schedule:\n{timer}"
    );
    // No behavioral tunable is ever baked into the timer unit - every knob
    // stays in borg.yml, read by the service's ExecStart at fire time.
    for baked in [
        "min-msgs",
        "min_msgs",
        "token",
        "mode",
        "ExecStart",
        "--since",
        "--limit",
        "clyde",
    ] {
        assert!(
            !timer.contains(baked),
            "the timer unit must not bake in `{baked}`:\n{timer}"
        );
    }
}

#[test]
fn schedule_change_is_the_only_timer_difference() {
    // Two configs differing only by schedule produce timers differing only in
    // the OnCalendar line - proof the cadence is the sole timer-resident knob.
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (_s1, t1) = render_units(&home, &binary, &cfg("daily"));
    let (_s2, t2) = render_units(&home, &binary, &cfg("weekly"));
    let diff1: Vec<&str> = t1.lines().filter(|l| !t2.contains(*l)).collect();
    let diff2: Vec<&str> = t2.lines().filter(|l| !t1.contains(*l)).collect();
    assert_eq!(diff1, vec!["OnCalendar=daily"]);
    assert_eq!(diff2, vec!["OnCalendar=weekly"]);
}

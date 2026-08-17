#![allow(clippy::unwrap_used)]

use super::*;
use clap::Parser;

// Wrap HarvestCliArgs in a minimal Parser so we can drive try_parse_from
// without standing up the entire sb CLI tree (same harness pattern as
// sb/src/cli/cortex/tests.rs).
#[derive(Parser)]
struct HarvestHarness {
    #[command(flatten)]
    harvest: HarvestCliArgs,
}

// One test, not three: the env-var leg mutates process environment, and a
// parallel sibling asserting the None default would race it. Sequential
// within a single test body, the order is the precedence chain itself.
#[test]
fn harvest_dormant_after_flag_env_default_precedence() {
    // Default: no flag, no env -> None (harvest::run falls back to config).
    let h = HarvestHarness::try_parse_from(["sb"]).unwrap();
    assert_eq!(h.harvest.dormant_after, None);

    // Flag parses.
    let h = HarvestHarness::try_parse_from(["sb", "--dormant-after", "1d"]).unwrap();
    assert_eq!(h.harvest.dormant_after.as_deref(), Some("1d"));

    // SAFETY: test-local env mutation; no other test reads this variable.
    unsafe { std::env::set_var("BORG_HARVEST_DORMANT_AFTER", "2d") };
    let from_env = HarvestHarness::try_parse_from(["sb"]).unwrap();
    assert_eq!(from_env.harvest.dormant_after.as_deref(), Some("2d"));
    // An explicit flag beats the env var.
    let from_flag = HarvestHarness::try_parse_from(["sb", "--dormant-after", "1d"]).unwrap();
    assert_eq!(from_flag.harvest.dormant_after.as_deref(), Some("1d"));
    unsafe { std::env::remove_var("BORG_HARVEST_DORMANT_AFTER") };
}

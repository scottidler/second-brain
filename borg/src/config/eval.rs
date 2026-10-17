use serde::{Deserialize, Serialize};

use vault::systemd::EnvBootstrap;

/// Daily, after the 03:00 harvest has published its notes.
const DEFAULT_SCHEDULE: &str = "*-*-* 05:00:00";

/// The `eval:` block of `borg.yml`: what the `sb-eval.timer` run scores
/// (`sb borg eval --unscored`).
///
/// `judge-models` is the required-check set's judge half: a trace is covered
/// only when every deterministic check and every judge check of every model
/// named here has a current, error-free row in `eval_scores`. Adding the
/// second judge is adding its model here. There is no default model: an empty
/// list makes `--unscored` fail loudly, because a silently chosen judge is a
/// silently chosen definition of "covered".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct EvalConfig {
    /// systemd `OnCalendar` expression for `sb-eval.timer`, the ONE value
    /// baked into the timer unit.
    pub schedule: String,
    /// Cap on traces one `--unscored` run scores; the rest stay in the
    /// backlog for the next run. `None` scores the whole backlog.
    pub limit: Option<usize>,
    /// Judge models (fabric `-m`), each scoring every trace.
    pub judge_models: Vec<String>,
    /// Secret bootstrap for `sb-eval.service` (same shape as
    /// `harvest.env-bootstrap`): its own `env-file`, so the one-shot run never
    /// clobbers a daemon's captured environment. It must carry every var
    /// `llm.api-keys` names; installing the unit without it while
    /// `llm.api-keys` is set is an error, because the run would score with no
    /// keys.
    pub env_bootstrap: Option<EnvBootstrap>,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            schedule: DEFAULT_SCHEDULE.to_string(),
            limit: None,
            judge_models: Vec::new(),
            env_bootstrap: None,
        }
    }
}

//! Herdr agent integration setup and diagnostics.
//!
//! Quota attribution starts from the session id Herdr reports for a pane, and
//! Herdr only learns that id once its integration for that agent is installed.
//! Without it a pane is detected but carries no session, so this plugin can
//! never attribute it and silently shows nothing. That is the least obvious
//! way for a fresh install to look broken, so `configure` says so out loud.

use crate::model::Harness;

/// Herdr's integration id for a harness, when it has one.
fn integration_id(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::Claude => Some("claude"),
        Harness::Codex => Some("codex"),
        Harness::Cursor => Some("cursor"),
    }
}

pub fn report_missing(agents: &[Harness]) {
    let Some(status) = read_status() else {
        return;
    };
    for harness in agents {
        let Some(id) = integration_id(*harness) else {
            continue;
        };
        if !is_missing(&status, id) {
            continue;
        }
        println!(
            "Herdr's {id} integration is not installed, so Herdr reports no session id for {id} panes and their quota cannot be attributed. Install it with `herdr integration install {id}`, then restart that agent pane."
        );
    }
}

fn read_status() -> Option<String> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = std::process::Command::new(executable)
        .args(["integration", "status"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Herdr prints one `<id>: <state> (<path>)` line per integration. Only an
/// explicit "not installed" is actionable; an unknown id or a reworded state
/// stays quiet rather than nagging about something that may be fine.
fn is_missing(status: &str, id: &str) -> bool {
    status.lines().any(|line| {
        line.trim()
            .strip_prefix(id)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|state| state.trim_start().starts_with("not installed"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
claude: current (v7) (/home/u/.claude/hooks/herdr-agent-state.sh)
codex: not installed (/home/u/.codex/herdr-agent-state.sh)
";

    #[test]
    fn only_an_explicit_not_installed_line_is_reported() {
        assert!(is_missing(STATUS, "codex"));
        assert!(!is_missing(STATUS, "claude"));
        assert!(!is_missing(STATUS, "kimi"));
        assert!(!is_missing("", "codex"));
    }

    #[test]
    fn a_prefix_match_is_not_a_hit() {
        // "cod" must not match the "codex:" line.
        assert!(!is_missing(STATUS, "cod"));
    }

    #[test]
    fn session_backed_harnesses_report_their_integration_id() {
        assert_eq!(integration_id(Harness::Claude), Some("claude"));
        assert_eq!(integration_id(Harness::Codex), Some("codex"));
    }
}

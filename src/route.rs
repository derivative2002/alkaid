use crate::herdr::AgentPane;
use crate::model::{BillingTarget, ContextUsage, Resolution};

/// Attribute a pane to a subscription from local evidence only.
///
/// Claude and Codex are the only collectors, and each harness maps 1:1 onto
/// its collector; an unknown harness is [`Resolution::Indeterminate`]. This
/// never infers a pane from the number of credentials on disk.
pub fn resolve(pane: &AgentPane) -> Resolution {
    pane.harness
        .billing()
        .map(BillingTarget::original_four)
        .map(Resolution::Subscription)
        .unwrap_or(Resolution::Indeterminate)
}

/// A resolved pane's routing facts.
///
/// Identity and per-session context come from the collector's own snapshot in
/// [`crate::refresh`]; direct collectors leave them unset here.
pub struct ResolvedPane {
    pub resolution: Resolution,
    pub identity: Option<crate::herdr::PaneIdentity>,
    pub context: Option<ContextUsage>,
}

pub fn resolve_with_identity(pane: &AgentPane) -> ResolvedPane {
    ResolvedPane {
        resolution: resolve(pane),
        identity: None,
        context: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{AgentPane, AgentSession, AgentStatus};
    use crate::model::{Harness, Provider};
    use std::collections::BTreeMap;

    fn pane(harness: Harness, session_id: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: "w1:p9".to_string(),
            workspace_id: "w1".to_string(),
            harness,
            session: session_id.map(|value| AgentSession {
                kind: Some("id".to_string()),
                value: value.to_string(),
            }),
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        }
    }

    #[test]
    fn collector_panes_resolve_to_canonical_targets() {
        for (harness, provider) in [
            (Harness::Claude, Provider::Claude),
            (Harness::Codex, Provider::Codex),
        ] {
            assert_eq!(
                resolve(&pane(harness, Some("thread-1"))),
                Resolution::Subscription(BillingTarget::original_four(provider))
            );
        }
    }

    #[test]
    fn a_session_id_does_not_change_attribution() {
        assert_eq!(
            resolve(&pane(Harness::Claude, Some("s1"))),
            resolve(&pane(Harness::Claude, None))
        );
    }
}

//! Seat identity from a Herdr pane label.
//!
//! The label is the machine-read name of naming-spec §5 (v2.2):
//! `<role>.<container>.<task>.<gen>[.<model>][@<machine>]`. Role and model
//! short name are read from it and from nothing else: the sidebar's
//! `quota_provider_model` can carry a borrowed provider-level model, so it is
//! never evidence of what a seat runs. A label that does not parse yields no
//! identity at all rather than a guess.

/// What the identity rows show for one seat. Empty means "not reported".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatIdentity {
    /// 统筹 / 秘书 / hub / spoke / subhub / svc.
    pub role: String,
    /// The label's model segment, verbatim (`opus-5-5`, `ds-v4-1-flash`).
    pub model: String,
    /// The container segment: the project line (`alioth`, `dubhe`).
    pub line: String,
    /// The task segment (`统筹`, `侧栏开发`).
    pub task: String,
}

/// Seats named by their task rather than their role segment. naming-spec
/// §5 v2.2: the coordinator and the secretary are `hub.alioth.<task>…`, and
/// which one a hub is lives in the task segment.
const TASK_ROLES: [&str; 2] = ["统筹", "秘书"];

pub fn from_label(label: &str) -> SeatIdentity {
    parse(label).unwrap_or_default()
}

fn parse(label: &str) -> Option<SeatIdentity> {
    let core = label.trim().split('@').next()?;
    let segments = core.split('.').collect::<Vec<_>>();
    let (role, container, task, gen, model) = match segments.as_slice() {
        [role, container, task, gen] => (*role, *container, *task, *gen, None),
        [role, container, task, gen, model] => (*role, *container, *task, *gen, Some(*model)),
        _ => return None,
    };
    if !is_kebab(container) || task.is_empty() || task.chars().any(char::is_whitespace) {
        return None;
    }
    if !is_gen(gen) || model.is_some_and(|model| !is_kebab(model)) {
        return None;
    }
    let role = match role {
        "hub" | "subhub" => TASK_ROLES
            .into_iter()
            .find(|name| task.starts_with(name))
            .unwrap_or(role),
        "spk" => "spoke",
        "svc" => "svc",
        _ => return None,
    };
    Some(SeatIdentity {
        role: role.to_string(),
        model: model.unwrap_or_default().to_string(),
        line: container.to_string(),
        task: task.to_string(),
    })
}

fn is_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// `MMDD-HHMM`, or `YYYYMMDD-HHMM` across a year boundary.
fn is_gen(value: &str) -> bool {
    let Some((date, time)) = value.split_once('-') else {
        return false;
    };
    matches!(date.len(), 4 | 8)
        && time.len() == 4
        && date.chars().chain(time.chars()).all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(role: &str, model: &str) -> (String, String) {
        (role.to_string(), model.to_string())
    }

    fn role_model(label: &str) -> (String, String) {
        let identity = from_label(label);
        (identity.role, identity.model)
    }

    #[test]
    fn line_and_task_are_the_container_and_task_segments() {
        let identity = from_label("hub.alioth.侧栏开发.0926-0036.opus-5-5@cpu-box");
        assert_eq!(identity.line, "alioth");
        assert_eq!(identity.task, "侧栏开发");
    }

    #[test]
    fn coordinator_and_secretary_come_from_the_task_segment() {
        assert_eq!(
            role_model("hub.alioth.统筹.0925-2307.opus-5-5@cpu-box"),
            id("统筹", "opus-5-5")
        );
        assert_eq!(role_model("hub.alioth.秘书.0921-1800"), id("秘书", ""));
    }

    #[test]
    fn role_segments_map_to_the_sidebar_words() {
        assert_eq!(
            role_model("hub.alioth.侧栏开发.0926-0036.opus-5-5@cpu-box"),
            id("hub", "opus-5-5")
        );
        assert_eq!(
            role_model("spk.dubhe.audit复核.0831-2139.ds-v4-1-flash"),
            id("spoke", "ds-v4-1-flash")
        );
        assert_eq!(role_model("svc.dubhe.gateway.0902-0400"), id("svc", ""));
        assert_eq!(
            role_model("subhub.lab.夜巡.20261231-2359.glm-5-3"),
            id("subhub", "glm-5-3")
        );
    }

    #[test]
    fn anything_off_spec_is_not_reported_rather_than_guessed() {
        for label in [
            "",
            "1",
            "claude",
            "hub.alioth.main.0817-fable",
            "hub.alioth.统筹.0925",
            "hub.Alioth.统筹.0925-2307",
            "hub.alioth.统筹.0925-2307.Opus 5.5",
            "boss.alioth.统筹.0925-2307",
            "hub.alioth..0925-2307",
            "hub.alioth.统筹.0925-2307.opus-5-5.extra",
        ] {
            assert_eq!(from_label(label), SeatIdentity::default(), "{label:?}");
        }
    }
}

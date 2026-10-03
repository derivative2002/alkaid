//! Custom Agent-panel groups the user names (owner 0928: "按项目线分，再加自己
//! 点名成组").
//!
//! The rules live in one file, `groups`, in the plugin config directory next
//! to the preferences. It is user data, so it is not in [`crate::prefs::ALL`]:
//! neither install nor uninstall rewrites it. One line per group; the line
//! order is the order the groups appear in, all of them above the Spaces:
//!
//! ```text
//! # 组名 = 选择器, 选择器, …
//! 中枢 = role:统筹, role:秘书, role:hub
//! ```
//!
//! Selectors read the pane label only (naming-spec §5), the same evidence the
//! identity rows use: `role:` (统筹 / 秘书 / hub / subhub / spoke / svc; 统筹
//! and 秘书 come from the task segment's prefix), `line:` (container segment),
//! `task:` (task segment) and `label:` (the whole label, to name one seat —
//! it carries the generation and model, so a relaunch under a new label
//! drops out). A comma separates alternatives; `&` joins selectors that must
//! all hold (`line:alioth&task:统筹`). The first group a seat matches wins; a
//! seat that matches none, or whose label is off-spec, stays in its Space.
//!
//! One name is reserved: a line named `置顶` pins its seats above everything
//! in every order (owner 0928: "统筹常驻时候可以固定在最上面"), outside any
//! group header; `置顶 = line:alioth&role:统筹`.
//!
//! The file takes effect only as a whole (Codex r5 g1 §6): one bad line and
//! the plugin keeps the last version that parsed cleanly (a copy in the
//! plugin state directory), or no custom groups if there never was one, so a
//! half-saved edit cannot regroup every seat. `herdr-agent-quota groups`
//! reports the problems by line number. A missing or empty file means
//! "Spaces only".

use crate::identity::SeatIdentity;

/// The rules file, in the plugin config directory.
pub const FILE: &str = "groups";

/// The reserved group name that pins its seats to the top.
pub const PIN_NAME: &str = "置顶";

/// At most this many groups: the group key is two digits.
pub const MAX_GROUPS: usize = 100;

const ROLES: [&str; 6] = ["统筹", "秘书", "hub", "subhub", "spoke", "svc"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selector {
    Role(String),
    Line(String),
    Task(String),
    Label(String),
}

impl Selector {
    fn parse(text: &str) -> Result<Self, String> {
        let (kind, value) = text
            .split_once(':')
            .map(|(kind, value)| (kind.trim(), value.trim()))
            .ok_or_else(|| format!("「{text}」缺少冒号（应为 role:/line:/task:/label:）"))?;
        if value.is_empty() {
            return Err(format!("「{text}」冒号后为空"));
        }
        match kind {
            "role" => {
                let role = if value == "spk" { "spoke" } else { value };
                if ROLES.contains(&role) {
                    Ok(Self::Role(role.to_string()))
                } else {
                    Err(format!(
                        "角色「{value}」不认识（可用：{}）",
                        ROLES.join(" ")
                    ))
                }
            }
            "line" => Ok(Self::Line(value.to_string())),
            "task" => Ok(Self::Task(value.to_string())),
            "label" => Ok(Self::Label(value.to_string())),
            _ => Err(format!(
                "选择器「{kind}」不认识（可用：role line task label）"
            )),
        }
    }

    fn matches(&self, label: &str, identity: &SeatIdentity) -> bool {
        match self {
            Self::Role(role) => identity.role == *role,
            Self::Line(line) => identity.line == *line,
            Self::Task(task) => identity.task == *task,
            Self::Label(whole) => label.trim() == whole,
        }
    }
}

/// Alternatives, each a list of selectors that must all hold.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    name: String,
    clauses: Vec<Vec<Selector>>,
}

/// The parsed rules, plus one message per skipped line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Groups {
    groups: Vec<Group>,
    /// The `置顶` line's alternatives; empty when there is none.
    pinned: Vec<Vec<Selector>>,
    pub problems: Vec<String>,
}

impl Groups {
    /// The rules in effect: the installed file when it parses cleanly (and
    /// then also kept as the last good copy), else the last good copy, else
    /// none.
    pub fn load() -> Self {
        let Some(text) = crate::prefs::read(FILE) else {
            return Self::default();
        };
        let parsed = Self::parse(&text);
        let last_good = last_good_path();
        if parsed.problems.is_empty() {
            if let Some(path) = &last_good {
                if std::fs::read_to_string(path).ok().as_deref() != Some(text.as_str()) {
                    let temporary = path.with_extension("tmp");
                    if std::fs::write(&temporary, &text).is_ok() {
                        let _ = std::fs::rename(&temporary, path);
                    }
                }
            }
            return parsed;
        }
        last_good
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .filter(|kept| kept.problems.is_empty())
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        let mut pinned: Option<Vec<Vec<Selector>>> = None;
        let mut problems = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let number = number + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, selectors)) = line.split_once('=') else {
                problems.push(format!("groups 第 {number} 行：缺少「=」"));
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                problems.push(format!("groups 第 {number} 行：组名为空"));
                continue;
            }
            if groups.iter().any(|group| group.name == name)
                || (name == PIN_NAME && pinned.is_some())
            {
                problems.push(format!("groups 第 {number} 行：组名「{name}」重复"));
                continue;
            }
            if name != PIN_NAME && groups.len() == MAX_GROUPS {
                problems.push(format!("groups 第 {number} 行：超过 {MAX_GROUPS} 组"));
                continue;
            }
            let parsed = selectors
                .split([',', '，'])
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|clause| {
                    clause
                        .split('&')
                        .map(str::trim)
                        .map(Selector::parse)
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>();
            match parsed {
                Ok(clauses) if clauses.is_empty() => {
                    problems.push(format!("groups 第 {number} 行：没有选择器"));
                }
                Ok(clauses) if name == PIN_NAME => pinned = Some(clauses),
                Ok(clauses) => groups.push(Group {
                    name: name.to_string(),
                    clauses,
                }),
                Err(message) => problems.push(format!("groups 第 {number} 行：{message}")),
            }
        }
        Self {
            groups,
            pinned: pinned.unwrap_or_default(),
            problems,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.pinned.is_empty()
    }

    /// The first group this seat matches, by its pane label.
    pub fn group_of(&self, label: &str) -> Option<usize> {
        let identity = crate::identity::from_label(label);
        if identity.role.is_empty() {
            return None;
        }
        self.groups
            .iter()
            .position(|group| any_clause(&group.clauses, label, &identity))
    }

    /// Does the `置顶` line claim this seat?
    pub fn is_pinned(&self, label: &str) -> bool {
        let identity = crate::identity::from_label(label);
        !identity.role.is_empty() && any_clause(&self.pinned, label, &identity)
    }

    pub fn name(&self, index: usize) -> Option<&str> {
        self.groups.get(index).map(|group| group.name.as_str())
    }

    /// The `al_grp` value for a group: two digits, so Herdr's text order is
    /// the file order.
    pub fn key(index: usize) -> String {
        format!("{index:02}")
    }
}

fn any_clause(clauses: &[Vec<Selector>], label: &str, identity: &SeatIdentity) -> bool {
    clauses.iter().any(|clause| {
        clause
            .iter()
            .all(|selector| selector.matches(label, identity))
    })
}

/// Where the last cleanly parsed rules are kept.
fn last_good_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(|directory| std::path::PathBuf::from(directory).join("groups.last-good"))
}

/// `herdr-agent-quota groups`: problems by line, then each group with the
/// seats it would claim, then whether the file is in effect.
pub fn check(file: Option<&std::path::Path>) -> anyhow::Result<()> {
    let text = match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("read {}: {error}", path.display()))?,
        None => crate::prefs::read(FILE).unwrap_or_default(),
    };
    let groups = Groups::parse(&text);
    for problem in &groups.problems {
        println!("{problem}");
    }
    let labels = crate::herdr::list_pane_labels().unwrap_or_default();
    if !groups.pinned.is_empty() {
        let pinned = labels
            .values()
            .filter(|label| groups.is_pinned(label))
            .map(String::as_str)
            .collect::<Vec<_>>();
        println!("{PIN_NAME}（{} 席）", pinned.len());
        for label in pinned {
            println!("  {label}");
        }
    }
    for (index, group) in groups.groups.iter().enumerate() {
        let members = labels
            .values()
            .filter(|label| groups.group_of(label) == Some(index))
            .map(String::as_str)
            .collect::<Vec<_>>();
        println!("{}（{} 席）", group.name, members.len());
        for label in members {
            println!("  {label}");
        }
    }
    if groups.problems.is_empty() {
        println!("可生效：{} 组。", groups.groups.len());
    } else {
        println!(
            "不生效：有 {} 处问题，插件继续用上一份无错的规则。",
            groups.problems.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COORD: &str = "hub.alioth.统筹.0925-2307.opus-5-5@cpu-box";
    const SEC: &str = "hub.alioth.秘书.0925-2308.opus-5-5@cpu-box";
    const HUB: &str = "hub.dubhe.main.0925-2308.fable-5-1@cpu-box";
    const SPOKE: &str = "spk.dubhe.归档打分.0925-2305.ds-v41-flash@cpu-box";

    #[test]
    fn the_owners_example_puts_coordinator_secretary_and_hubs_in_one_group() {
        let groups = Groups::parse("# 中枢\n中枢 = role:统筹, role:秘书, role:hub\n");
        assert!(groups.problems.is_empty(), "{:?}", groups.problems);
        for label in [COORD, SEC, HUB] {
            assert_eq!(groups.group_of(label), Some(0), "{label}");
        }
        assert_eq!(groups.group_of(SPOKE), None);
        assert_eq!(groups.name(0), Some("中枢"));
    }

    #[test]
    fn the_first_matching_group_wins_and_order_is_the_file_order() {
        let groups = Groups::parse(
            "天枢线 = line:dubhe\n中枢 = role:hub，role:统筹\n点名 = label:hub.alioth.秘书.0925-2308.opus-5-5@cpu-box",
        );
        assert!(groups.problems.is_empty(), "{:?}", groups.problems);
        assert_eq!(
            groups.group_of(HUB),
            Some(0),
            "dubhe hub goes to the earlier line group"
        );
        assert_eq!(groups.group_of(SPOKE), Some(0));
        assert_eq!(groups.group_of(COORD), Some(1));
        assert_eq!(groups.group_of(SEC), Some(2));
        assert_eq!(Groups::key(2), "02");
    }

    #[test]
    fn ampersand_joins_selectors_that_must_all_hold() {
        let groups = Groups::parse("本线统筹 = line:alioth&task:统筹, line:dubhe&role:hub");
        assert!(groups.problems.is_empty(), "{:?}", groups.problems);
        assert_eq!(groups.group_of(COORD), Some(0));
        assert_eq!(groups.group_of(HUB), Some(0));
        assert_eq!(groups.group_of(SEC), None);
        assert_eq!(groups.group_of(SPOKE), None);
        assert!(!Groups::parse("坏 = line:alioth&").problems.is_empty());
    }

    #[test]
    fn the_reserved_name_pins_and_is_not_a_group() {
        let groups = Groups::parse("置顶 = line:alioth&role:统筹\n中枢 = role:统筹, role:hub");
        assert!(groups.problems.is_empty(), "{:?}", groups.problems);
        assert!(groups.is_pinned(COORD));
        assert!(!groups.is_pinned(SEC));
        assert!(!groups.is_pinned("claude"));
        assert_eq!(groups.name(0), Some("中枢"));
        assert_eq!(groups.name(1), None);
        assert!(!Groups::parse("置顶 = role:统筹").is_empty());
        let twice = Groups::parse("置顶 = role:统筹\n置顶 = role:秘书");
        assert_eq!(twice.problems.len(), 1);
    }

    #[test]
    fn task_and_spk_alias_select_by_segment() {
        let groups = Groups::parse("打分 = task:归档打分\n辐条 = role:spk");
        assert_eq!(groups.group_of(SPOKE), Some(0));
        assert_eq!(
            groups.group_of("spk.lab.巡检.0926-1200.ds-v41-flash@cpu-box"),
            Some(1)
        );
    }

    #[test]
    fn a_bad_line_is_skipped_and_reported_by_number() {
        let groups = Groups::parse(
            "中枢 = role:hub\n没有等号\n = role:hub\n中枢 = role:spoke\n坏 = role:boss\n空 = \n怪 = color:red\n好 = line:egress",
        );
        assert_eq!(groups.name(0), Some("中枢"));
        assert_eq!(groups.name(1), Some("好"));
        assert_eq!(groups.name(2), None);
        let lines = groups
            .problems
            .iter()
            .map(|problem| problem.split('：').next().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "groups 第 2 行",
                "groups 第 3 行",
                "groups 第 4 行",
                "groups 第 5 行",
                "groups 第 6 行",
                "groups 第 7 行"
            ]
        );
    }

    #[test]
    fn an_off_spec_label_is_never_grouped() {
        let groups = Groups::parse("全部 = role:hub, line:alioth");
        assert_eq!(groups.group_of("claude"), None);
        assert_eq!(groups.group_of(""), None);
        assert!(Groups::parse("").is_empty());
        assert!(Groups::parse("# only a comment\n").is_empty());
    }
}

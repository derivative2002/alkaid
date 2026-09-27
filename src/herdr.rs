use crate::model::{ContextUsage, Harness, Provider};
use crate::presentation::{MetadataTokens, RowStyle, SidebarShape};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const METADATA_TTL_MS: &str = "86400000";
const MAX_METADATA_TOKENS: usize = 16;
/// Every name [`desired_tokens`] can produce, and nothing else.
///
/// This list is the comparison set for [`metadata_matches`] and the report set
/// for [`metadata_report_names`]. A name that is listed but never produced is
/// not free: it is compared on every refresh and it competes for Herdr's
/// 16-token report budget. Add a name here only together with the field that
/// fills it.
const METADATA_TOKEN_NAMES: [&str; 44] = [
    "quota_group",
    "quota_pad",
    "quota_icon",
    "quota_icon_working",
    "quota_icon_done",
    "quota_provider",
    "quota_model",
    "quota_provider_model",
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
    "quota_cache",
    "quota_cache_ttl",
    "quota_cache_state",
    "quota_5h_normal",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week_normal",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_month_normal",
    "quota_month_warning",
    "quota_month_danger",
    "quota_month_unknown",
    "quota_topic",
    "quota_error",
    HEADROOM_TOKEN,
    ROLE_TOKEN,
    MODEL_TOKEN,
    SPACE_TOKEN,
    SORT_TOKEN,
    GAP_TOKEN,
    CHANNEL_TOKEN,
    MODEL_MARK_TOKEN,
    GROUP_TOKEN,
    ROLE_RANK_TOKEN,
    ATTENTION_TOKEN,
];
/// A blank row after a Space's last pane (group styles `rule` and `count`).
/// It sits at the end of that pane's rows rather than above the next header:
/// Herdr hang-indents every row after a pane's first, so a blank first row
/// would push the header itself in.
pub(crate) const GAP_TOKEN: &str = "al_gap";
/// Channel mark: 天枢 gateway or official (logo layouts only).
pub(crate) const CHANNEL_TOKEN: &str = "al_channel";
/// Model-family mark from the session's reported model (logo layouts only).
pub(crate) const MODEL_MARK_TOKEN: &str = "al_model_mark";
/// Group key on every seat: `C000`, `C001`, … for the user's custom groups
/// (`crate::groups`, file order), `Z` for a seat that stays in its Space. The
/// grouped views sort on it first, so custom groups sit above the Spaces.
/// Every seat carries a value — never a missing token — so leaving a custom
/// group is an explicit replacement, not a token that may linger (Codex r5
/// g1 §1). It does not depend on the order, so toggling writes nothing.
pub(crate) const GROUP_TOKEN: &str = "al_grp";
/// The value of [`GROUP_TOKEN`] for a seat in its Space.
const SPACE_GROUP: &str = "Z";
/// Attention rank that follows the icon colour (`0` blocked, `1` teal unseen
/// completion, `2` yellow working, `3` white), for the `attention` order.
/// Herdr's built-in `attention` can disagree with the plugin's teal (a
/// same-tab sibling finishes as server `idle` while its ring is still teal),
/// so the order sorts on the colour the user actually sees (owner 0928: green
/// first, a clicked one sinks, one that turns yellow moves down). It rides
/// on every icon report, so it adds no write.
pub(crate) const ATTENTION_TOKEN: &str = "al_att";
/// Role rank for the `role` order (`0` 统筹 … `5` svc, `9` unreported), from
/// the label's role like `al_role`, so it moves only when the label does.
pub(crate) const ROLE_RANK_TOKEN: &str = "al_role_rank";

/// Everything about how a pass draws its rows that is not per-pane data.
pub(crate) struct Look {
    pub style: crate::icons::IconStyle,
    pub group: crate::look::GroupStyle,
    pub logos: crate::look::LogoLayout,
    /// Which rows the installed template shows; decides what a report over
    /// Herdr's token budget may defer.
    pub rows: crate::cli::SidebarRows,
    /// Per workspace, from the same inventory the pass publishes against.
    pub counts: BTreeMap<String, crate::look::SpaceCounts>,
    /// The last pane of each Space (largest sort key), when gapped.
    pub tails: BTreeSet<String>,
}

impl Look {
    /// The installed look for one pass.
    ///
    /// Counts use the statuses this pass publishes — `publishing` has the
    /// plugin's unseen set folded in, so a count agrees with the teal icon —
    /// and the raw inventory for every other pane.
    fn installed(
        order: crate::cli::AgentOrder,
        grouping: &Grouping,
        inventory: &[AgentPane],
        publishing: &[AgentPane],
        tokens: &[PaneTokens],
    ) -> Self {
        let group = crate::look::GroupStyle::installed();
        let folded = inventory
            .iter()
            .map(|pane| {
                publishing
                    .iter()
                    .find(|published| published.pane_id == pane.pane_id)
                    .unwrap_or(pane)
                    .clone()
            })
            .collect::<Vec<_>>();
        let tails = space_tails(order, group, grouping, inventory, publishing, tokens);
        Self {
            style: crate::icons::IconStyle::installed(),
            group,
            logos: crate::look::LogoLayout::installed(),
            rows: crate::configure::resolved_sidebar_rows(None),
            counts: space_counts(grouping, &folded),
            tails,
        }
    }

    #[cfg(test)]
    fn plain() -> Self {
        Self {
            style: crate::icons::IconStyle::Glyph,
            group: crate::look::GroupStyle::Plain,
            logos: crate::look::LogoLayout::None,
            rows: crate::cli::SidebarRows::Identity,
            counts: BTreeMap::new(),
            tails: BTreeSet::new(),
        }
    }

    /// The group header text a head pane carries; `gid` is
    /// [`Grouping::gid`].
    fn header(&self, gid: &str, name: &str) -> String {
        let counts = self.counts.get(gid).copied().unwrap_or_default();
        crate::look::header(self.group, name, counts)
    }
}

fn space_counts(
    grouping: &Grouping,
    inventory: &[AgentPane],
) -> BTreeMap<String, crate::look::SpaceCounts> {
    let mut counts: BTreeMap<String, crate::look::SpaceCounts> = BTreeMap::new();
    for pane in inventory {
        let gid = grouping.gid(pane);
        if gid.is_empty() {
            continue;
        }
        let entry = counts.entry(gid).or_default();
        entry.seats += 1;
        match pane.icon_status() {
            AgentStatus::Blocked => entry.blocked += 1,
            AgentStatus::Done => entry.done += 1,
            _ => {}
        }
    }
    counts
}
/// The Agent view's sort key: three-digit headroom (`999` when unknown) then
/// the pane id. The view sorts on this token and the Space head is the pane
/// with the smallest value, so both sides break a headroom tie the same way.
/// Sorting on `quota_headroom` alone left ties to Herdr's own tab order while
/// the head was chosen by pane id — two panes at `004` put the header on the
/// second one and the first one above it (0927, 统筹 over the alioth header).
pub(crate) const SORT_TOKEN: &str = "al_sort";

/// The one definition of a pane's place in its Space; see [`SORT_TOKEN`].
fn sort_key(headroom: u8, pane_id: &str) -> String {
    let headroom = if headroom > 100 {
        999
    } else {
        u16::from(headroom)
    };
    format!("{headroom:03}{pane_id}")
}

/// Role order for [`crate::cli::AgentOrder::Role`]: 统筹, 秘书, hub, subhub,
/// spoke, svc, then anything unreported.
fn role_rank(role: &str) -> char {
    match role {
        "统筹" => '0',
        "秘书" => '1',
        "hub" => '2',
        "subhub" => '3',
        "spoke" => '4',
        "svc" => '5',
        _ => '9',
    }
}

/// Which group each pane is in for one pass, and where the view lists it.
///
/// A group is one of the user's custom groups (`#C000`, … from
/// `crate::groups`) or, for every other seat, its Space (the workspace id).
/// Custom groups only form under an order that owns a grouped view (`quota`,
/// `role`); the `al_grp` value itself is written under every order.
///
/// Membership of a pane this pass labels comes from its label; any other pane
/// sits where the `al_grp` it carries puts it — the view sees tokens, not
/// rules. The head of a group is the pane the view lists first, so its key is
/// built from what Herdr compares: inside a custom group the workspace number
/// (Herdr's `workspace_order`), under `role` the role rank, then `al_sort`.
pub(crate) struct Grouping {
    order: crate::cli::AgentOrder,
    rules: crate::groups::Groups,
    /// Pane id → custom group (`None`: its Space), for panes labelled this
    /// pass.
    custom: BTreeMap<String, Option<usize>>,
    /// Pane id → role rank, for panes labelled this pass.
    roles: BTreeMap<String, char>,
    /// Workspace id → its number in Herdr's order.
    ranks: BTreeMap<String, u64>,
}

impl Grouping {
    fn new(
        order: crate::cli::AgentOrder,
        rules: crate::groups::Groups,
        labelled: &[AgentPane],
        pane_labels: Option<&BTreeMap<String, String>>,
        ranks: BTreeMap<String, u64>,
    ) -> Self {
        let mut custom = BTreeMap::new();
        let mut roles = BTreeMap::new();
        if let Some(labels) = pane_labels {
            for pane in labelled {
                let label = labels.get(&pane.pane_id).map(String::as_str).unwrap_or("");
                custom.insert(pane.pane_id.clone(), rules.group_of(label));
                let role = crate::identity::from_label(label).role;
                roles.insert(pane.pane_id.clone(), role_rank(&role));
            }
        }
        Self {
            order,
            rules,
            custom,
            roles,
            ranks,
        }
    }

    /// Spaces only, as before round five.
    #[cfg(test)]
    pub(crate) fn spaces(order: crate::cli::AgentOrder) -> Self {
        Self::new(
            order,
            crate::groups::Groups::default(),
            &[],
            None,
            BTreeMap::new(),
        )
    }

    /// The `al_grp` this pass writes for a pane, or `None` to keep what it
    /// carries (its label was not read).
    fn group_value(&self, pane: &AgentPane) -> Option<String> {
        self.custom.get(&pane.pane_id).map(|group| match group {
            Some(group) => format!("C{}", crate::groups::Groups::key(*group)),
            None => SPACE_GROUP.to_string(),
        })
    }

    /// The custom group the view puts this pane in, if any.
    fn custom_group(&self, pane: &AgentPane) -> Option<usize> {
        if !self.order.custom_groups() {
            return None;
        }
        let value = self
            .group_value(pane)
            .or_else(|| pane.tokens.get(GROUP_TOKEN).cloned())?;
        value.strip_prefix('C')?.parse().ok()
    }

    /// The group key: `#C…` for a custom group, else the workspace id (empty
    /// when the pane has none, which puts it in no group).
    fn gid(&self, pane: &AgentPane) -> String {
        match self.custom_group(pane) {
            Some(group) => format!("#C{}", crate::groups::Groups::key(group)),
            None => pane.workspace_id.clone(),
        }
    }

    /// The header text source for a group key.
    fn name(&self, gid: &str, workspace_labels: &BTreeMap<String, String>) -> Option<String> {
        if let Some(key) = gid.strip_prefix("#C") {
            return key
                .parse()
                .ok()
                .and_then(|group| self.rules.name(group))
                .map(str::to_string)
                .or_else(|| Some(gid.to_string()));
        }
        workspace_labels
            .get(gid)
            .cloned()
            .filter(|label| !label.is_empty())
            .or_else(|| (!gid.is_empty()).then(|| gid.to_string()))
    }

    /// The key a pane sits at inside its group, for choosing heads and
    /// tails. `headroom` is what this pass writes, `None` for a pane it does
    /// not write: that one sits where its own `al_sort` puts it, and Herdr
    /// lists a pane without the token last.
    fn place(&self, pane: &AgentPane, headroom: Option<u8>) -> String {
        let sort = headroom
            .map(|headroom| sort_key(headroom, &pane.pane_id))
            .or_else(|| pane.tokens.get(SORT_TOKEN).cloned())
            .unwrap_or_else(|| format!("{UNSORTED}{}", pane.pane_id));
        let mut key = String::new();
        if self.custom_group(pane).is_some() {
            // An unknown workspace number sorts last, like Herdr's own.
            let rank = self.ranks.get(&pane.workspace_id).copied().unwrap_or(999);
            key.push_str(&format!("{rank:03}"));
        }
        if self.order == crate::cli::AgentOrder::Role {
            let role = self
                .roles
                .get(&pane.pane_id)
                .copied()
                .or_else(|| {
                    pane.tokens
                        .get(ROLE_RANK_TOKEN)
                        .and_then(|rank| rank.chars().next())
                })
                .unwrap_or(UNSORTED);
            key.push(role);
        }
        key.push_str(&sort);
        key
    }
}
/// Seat role parsed from the pane label (naming-spec §5): 统筹 / 秘书 / hub /
/// spoke / subhub / svc. Changes only when the label does.
pub(crate) const ROLE_TOKEN: &str = "al_role";
/// The pane label's model segment, verbatim. Never read from
/// `quota_provider_model`, which can carry a borrowed provider-level model.
pub(crate) const MODEL_TOKEN: &str = "al_model";
/// The Space name on each seat, published only when the panel is not
/// grouped by Space — grouped, the `$quota_group` header already says it.
pub(crate) const SPACE_TOKEN: &str = "al_space";
/// Sort key for Herdr's Agent view: remaining quota as a zero-padded percent
/// (`007`), so Herdr's ordering of the token values is also their numeric
/// ordering.
///
/// Published for every pane whose quota is known, whether or not the user
/// chose `--agent-order quota`, because it never renders: no sidebar row
/// references it. Publishing it unconditionally is what makes changing the
/// order a Herdr-side toggle instead of a metadata write to every pane, and it
/// costs no extra writes — the value only moves when a quota token beside it
/// moves anyway.
pub(crate) const HEADROOM_TOKEN: &str = "quota_headroom";
/// The subset of [`METADATA_TOKEN_NAMES`] whose value comes from the cached
/// quota windows and nothing else. [`quota_rows_have_drifted`] compares these,
/// so a name added here must be one a snapshot alone can render.
const QUOTA_WINDOW_TOKEN_NAMES: [&str; 17] = [
    "quota_5h_normal",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week_normal",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_month_normal",
    "quota_month_warning",
    "quota_month_danger",
    "quota_month_unknown",
    HEADROOM_TOKEN,
];
/// Names a pane may still carry from an older build of this plugin. They are
/// never produced again, so a report clears them until the pane is clean.
const OBSOLETE_METADATA_TOKEN_NAMES: [&str; 14] = [
    "quota_state",
    "quota_status",
    "quota_summary",
    "quota_5h",
    "quota_5h_label",
    "quota_5h_percent",
    "quota_5h_eta",
    "quota_5h_caution",
    "quota_week",
    "quota_week_label",
    "quota_week_percent",
    "quota_week_eta",
    "quota_week_caution",
    "quota_week_inline_caution",
];
const LEGACY_METADATA_TOKEN_NAMES: [&str; 4] = [
    "quota_badge",
    "quota_session",
    "quota_week_inline_label",
    "quota_week_inline_eta",
];
/// The names the context row can be published into, in the order a report
/// clears them. Herdr fixes a token's colour by name, so the only way to
/// colour the context row is to publish it into a name whose row template
/// already carries that colour — which is why `gauges` needs the three
/// severity variants and `packed`/`stacked` keep the plain one.
///
/// Exactly one is ever filled. Publishing a second would draw two context
/// rows in the same pane.
const CONTEXT_TOKEN_NAMES: [&str; 4] = [
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
];
/// Values that must reach the pane in the *same* report that changed them,
/// even when the budget is tight: the identity, the live diagnostics, and the
/// inline week variants, whose styling flips as soon as a 5h window appears.
const ROWS_THAT_MUST_NOT_LAG: [&str; 17] = [
    "quota_group",
    "quota_icon",
    "quota_provider",
    "quota_model",
    "quota_provider_model",
    "quota_topic",
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
    "quota_cache",
    "quota_cache_ttl",
    "quota_cache_state",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
];

/// [`ROWS_THAT_MUST_NOT_LAG`] for the identity rows (round two onward).
const IDENTITY_ROWS_THAT_MUST_NOT_LAG: [&str; 11] = [
    "quota_group",
    "quota_icon",
    SORT_TOKEN,
    GROUP_TOKEN,
    ROLE_RANK_TOKEN,
    GAP_TOKEN,
    CHANNEL_TOKEN,
    MODEL_MARK_TOKEN,
    ROLE_TOKEN,
    MODEL_TOKEN,
    SPACE_TOKEN,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub kind: Option<String>,
    pub value: String,
}

impl AgentSession {
    /// Existing Herdr integrations historically omitted `kind`; keep treating
    /// those values as opaque ids. A path is never exposed through this seam.
    pub fn id(&self) -> Option<&str> {
        self.kind
            .as_deref()
            .is_none_or(|kind| kind == "id")
            .then_some(self.value.as_str())
    }

    pub fn path(&self) -> Option<&str> {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind == "path")
            .then_some(self.value.as_str())
    }
}

/// Herdr's effective pane status from `agent_status`.
///
/// The CLI list maps idle+unseen to `done`, but same-tab completions are
/// often already `idle` on the server while the TUI ring is still teal.
/// Brand-icon colour therefore uses this enum after `refresh` has applied
/// the plugin's own unseen set — never a second `state_icon` ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentStatus {
    #[default]
    Idle,
    Working,
    Done,
    Blocked,
    Unknown,
}

impl AgentStatus {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "working" => Self::Working,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            "unknown" => Self::Unknown,
            _ => Self::Idle,
        }
    }

    pub fn is_working(self) -> bool {
        matches!(self, Self::Working)
    }

    /// Metadata token that carries the brand glyph for this status.
    fn icon_token(self) -> &'static str {
        match self {
            Self::Working => "quota_icon_working",
            Self::Done => "quota_icon_done",
            Self::Idle | Self::Blocked | Self::Unknown => "quota_icon",
        }
    }

    /// [`ATTENTION_TOKEN`] for an icon status: needs-you first, then the
    /// teal unseen completion, then yellow working, then white.
    fn attention_rank(self) -> &'static str {
        match self {
            Self::Blocked => "0",
            Self::Done => "1",
            Self::Working => "2",
            Self::Idle | Self::Unknown => "3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub harness: Harness,
    pub session: Option<AgentSession>,
    pub session_summary: String,
    pub topic: String,
    pub tokens: BTreeMap<String, String>,
    /// From Herdr `agent_status`. Drives brand-icon colour and the watch pulse.
    pub status: AgentStatus,
    /// Herdr `focused` describes the current pane, not whether a completion
    /// was acknowledged by a later focus event.
    pub focused: bool,
    /// Herdr `state_labels`: the text its built-in `state_text` token prints
    /// for each status. Read back so an unchanged label set is never re-sent.
    pub state_labels: BTreeMap<String, String>,
}

impl AgentPane {
    pub fn working(&self) -> bool {
        self.status.is_working()
    }

    /// Status the brand icon should mirror.
    ///
    /// Working is yellow, unseen completion is teal, acknowledged is white.
    /// Callers fold the plugin unseen-set into `status` before publish. Do
    /// not treat a leftover `$quota_icon_done` token as unseen: a concurrent
    /// inventory read after mark-seen still carries that token and would
    /// paint teal back on.
    pub fn icon_status(&self) -> AgentStatus {
        if self.status.is_working() {
            return AgentStatus::Working;
        }
        if self.status == AgentStatus::Done {
            return AgentStatus::Done;
        }
        self.status
    }

    pub fn icon_needs_update(&self) -> bool {
        let status = self.icon_status();
        let active = status.icon_token();
        ICON_TOKEN_NAMES
            .into_iter()
            .any(|name| self.tokens.contains_key(name) != (name == active))
            || self.tokens.get(ATTENTION_TOKEN).map(String::as_str) != Some(status.attention_rank())
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub panes: Vec<AgentPane>,
    pub working_providers: Vec<Provider>,
    pub working_pane_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PaneIdentity {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub enum PaneQuotaUpdate {
    Replace(Box<MetadataTokens>),
    Clear,
    Preserve,
}

#[derive(Debug, Clone)]
pub struct PaneTokens {
    pub pane_id: String,
    pub quota: PaneQuotaUpdate,
    pub identity: Option<PaneIdentity>,
    pub context: Option<ContextUsage>,
}

/// Show one Herdr notification.
///
/// Failure is reported to the caller but is never worth aborting a publish
/// for: a missed toast costs the user nothing that the sidebar does not
/// already show.
pub fn notify(title: &str, body: &str) -> Result<()> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["notification", "show", title, "--body", body])
        .args(["--sound", "request"])
        .output()
        .context("show Herdr notification")?;
    if !output.status.success() {
        anyhow::bail!("Herdr notification failed with {}", output.status);
    }
    Ok(())
}

/// Source that owns this plugin's Herdr Agent view. Herdr requires the
/// `plugin:<id>` form and rejects a set whose plugin is missing or disabled.
const AGENT_VIEW_SOURCE: &str = "plugin:herdr-agent-quota";
/// A socket request must not outlive the event hook that sent it. Herdr
/// answers these in microseconds; anything near this is a hung server, and a
/// sidebar sort is never worth blocking a turn for.
const SOCKET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask Herdr to order its Agent panel the way `order` names.
///
/// Herdr keeps one Agent view and this replaces it. The default agent order
/// is `quota`, so configure and startup both call this unless the user
/// chose `default`. The view does not survive a server restart, which is
/// why the startup hook re-applies it.
///
/// `quota`: `workspace_order` keeps each Space contiguous — the same grouping
/// Herdr's own spaces sort uses — so quota ranking never scatters one
/// project's agents across the panel. Inside a space, `quota_headroom` ranks
/// tightest first.
///
/// `attention`: flat on purpose. Herdr's built-in `attention` rank ascends
/// idle < working < done < blocked (unknown below idle), so it is sorted
/// descending; `quota_headroom` breaks ties, and a pane without that token
/// sorts after the ones that have it. Both keys are Herdr-side values, so the
/// order follows a status change without a metadata write.
pub fn set_agent_view(order: crate::cli::AgentOrder) -> Result<()> {
    socket_request(&agent_view_request(order)).map(|_| ())
}

fn agent_view_request(order: crate::cli::AgentOrder) -> Value {
    use crate::cli::AgentOrder;
    let key = serde_json::json!({"field": {"token": SORT_TOKEN}, "order": "asc"});
    let sort = match order {
        // Custom groups first (a seat without `al_grp` sorts after them),
        // then Spaces by Herdr's own workspace order.
        AgentOrder::Quota | AgentOrder::Default => vec![
            serde_json::json!({"field": {"token": GROUP_TOKEN}, "order": "asc"}),
            serde_json::json!({"field": "workspace_order", "order": "asc"}),
            key,
        ],
        // Role within one Space: a custom group spanning Spaces still lists
        // them by workspace first (Codex r5 g1 §5).
        AgentOrder::Role => vec![
            serde_json::json!({"field": {"token": GROUP_TOKEN}, "order": "asc"}),
            serde_json::json!({"field": "workspace_order", "order": "asc"}),
            serde_json::json!({"field": {"token": ROLE_RANK_TOKEN}, "order": "asc"}),
            key,
        ],
        // By the colour the user sees (`al_att`), then 中枢 and the other
        // custom groups before the Spaces, then Space, then quota.
        AgentOrder::Attention => vec![
            serde_json::json!({"field": {"token": ATTENTION_TOKEN}, "order": "asc"}),
            serde_json::json!({"field": {"token": GROUP_TOKEN}, "order": "asc"}),
            serde_json::json!({"field": "workspace_order", "order": "asc"}),
            key,
        ],
        AgentOrder::Tightest => vec![key],
    };
    serde_json::json!({
        "id": "agent-quota:view-set",
        "method": "agent.view.set",
        "params": {
            "source": AGENT_VIEW_SOURCE,
            "label": order.label(),
            "sort": sort,
        },
    })
}

/// Give the Agent panel back to Herdr's own ordering.
///
/// Scoped to this plugin's source: a view someone else owns must survive.
pub fn clear_quota_agent_view() -> Result<()> {
    socket_request(&serde_json::json!({
        "id": "agent-quota:view-clear",
        "method": "agent.view.clear",
        "params": {"source": AGENT_VIEW_SOURCE},
    }))
    .map(|_| ())
}

/// One request, one reply, one connection.
///
/// `agent.view.*` has no CLI subcommand in Herdr 0.8, so this is the only
/// place the plugin speaks the raw socket protocol. Nothing here subscribes,
/// so no stream is ever held open — the replay and focus-storm problems that
/// come with `events.subscribe` do not apply.
///
/// Outside Herdr there is no socket and this is a no-op, exactly like
/// [`crate::prefs::write`], so a direct CLI run still works.
fn socket_request(payload: &Value) -> Result<Option<Value>> {
    use std::io::{BufRead, BufReader, Write};

    let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") else {
        return Ok(None);
    };
    let stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect to Herdr at {}", path.to_string_lossy()))?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let mut writer = &stream;
    writeln!(writer, "{payload}").context("send Herdr socket request")?;
    writer.flush().context("flush Herdr socket request")?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("read Herdr socket reply")?;
    let reply: Value = serde_json::from_str(&line).context("parse Herdr socket reply")?;
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Herdr rejected the request");
        anyhow::bail!("{message}");
    }
    Ok(Some(reply))
}

pub fn list_agent_panes() -> Result<Vec<AgentPane>> {
    Ok(list_agent_state()?.panes)
}

/// Read Herdr's agent inventory once and derive both panes and working
/// providers from that same response. The active-turn watcher uses this
/// combined view so one poll does not fan out into one `agent list` call per
/// provider.
pub fn list_agent_state() -> Result<AgentState> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
    panes.dedup_by(|left, right| left.pane_id == right.pane_id);
    let mut working_pane_ids = Vec::new();
    collect_working_providers(&value, &mut Vec::new(), &mut working_pane_ids);
    working_pane_ids.sort();
    working_pane_ids.dedup();
    Ok(AgentState {
        panes,
        working_providers: working_providers_from(&value),
        working_pane_ids,
    })
}

/// One pane from a single inventory read, for the event and focus paths.
pub fn find_agent_pane(pane_id: &str) -> Result<Option<AgentPane>> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    let Some(pane) = panes.into_iter().find(|pane| pane.pane_id == pane_id) else {
        return Ok(None);
    };
    Ok(Some(pane))
}

/// Return only the named agents from one inventory read. A focus change must
/// never acknowledge an unrelated green pane in the same tab.
pub fn find_agent_icon_panes(pane_ids: &[&str]) -> Result<Vec<AgentPane>> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    panes.retain(|pane| pane_ids.contains(&pane.pane_id.as_str()));
    panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
    panes.dedup_by(|left, right| left.pane_id == right.pane_id);
    Ok(panes)
}

fn list_agent_value() -> Result<Value> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["agent", "list"])
        .output()
        .context("list Herdr agents")?;
    if !output.status.success() {
        anyhow::bail!("Herdr agent list failed with {}", output.status);
    }
    serde_json::from_slice(&output.stdout).context("parse Herdr agent list")
}

/// Pane id and harness of the focused pane.
///
/// `pane.focused` carries no agent in its payload, so this is the only way to
/// learn which pane the user moved to. Herdr answers with the pane id, which
/// is what keeps `focus` scoped to exactly one pane.
pub fn current_focused_pane() -> Result<Option<(String, Harness)>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["pane", "current"])
        .output()
        .context("read focused Herdr pane")?;
    if !output.status.success() {
        anyhow::bail!("Herdr pane current failed with {}", output.status);
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("parse focused Herdr pane")?;
    let pane = value.pointer("/result/pane").unwrap_or(&value);
    let Some(pane_id) = pane.get("pane_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(pane
        .get("agent")
        .and_then(Value::as_str)
        .and_then(Harness::from_agent_name)
        .map(|harness| (pane_id.to_string(), harness)))
}

/// Resolve a workspace/tab focus event to that tab's focused pane. Ignore a
/// delayed event once the session has focused somewhere else.
pub fn focused_pane_in_snapshot(
    workspace_id: Option<&str>,
    tab_id: Option<&str>,
) -> Result<Option<String>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["api", "snapshot"])
        .output()
        .context("read Herdr focus snapshot")?;
    if !output.status.success() {
        anyhow::bail!("Herdr api snapshot failed with {}", output.status);
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("parse Herdr focus snapshot")?;
    let snapshot = value.pointer("/result/snapshot").unwrap_or(&value);
    if workspace_id
        .is_some_and(|id| snapshot.get("focused_workspace_id").and_then(Value::as_str) != Some(id))
        || tab_id
            .is_some_and(|id| snapshot.get("focused_tab_id").and_then(Value::as_str) != Some(id))
    {
        return Ok(None);
    }
    if workspace_id.is_none() && tab_id.is_none() {
        return Ok(snapshot
            .get("focused_pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned));
    }
    let tab_id = tab_id.or_else(|| {
        snapshot
            .get("workspaces")?
            .as_array()?
            .iter()
            .find(|workspace| {
                workspace.get("workspace_id").and_then(Value::as_str) == workspace_id
            })?
            .get("active_tab_id")?
            .as_str()
    });
    let Some(tab_id) = tab_id else {
        return Ok(None);
    };
    Ok(snapshot
        .get("layouts")
        .and_then(Value::as_array)
        .and_then(|layouts| {
            layouts
                .iter()
                .find(|layout| layout.get("tab_id").and_then(Value::as_str) == Some(tab_id))
        })
        .and_then(|layout| layout.get("focused_pane_id"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

// Reading a pane makes Herdr repaint it, which visibly scrolls the agent's
// terminal. Only the pane that fired the event is worth that cost; every other
// pane keeps the topic it last published.
pub fn refresh_pane_topic(pane: &mut AgentPane) {
    if !reads_screen_topic(pane.harness) {
        return;
    }
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    if let Some(topic) = read_pane_topic(&executable, pane) {
        pane.topic = topic;
    }
}

fn collect_agent_panes(value: &Value, panes: &mut Vec<AgentPane>) {
    match value {
        Value::Object(map) => {
            let pane_id = map
                .get("pane_id")
                .or_else(|| map.get("paneId"))
                .and_then(Value::as_str);
            let kind = map
                .get("agent")
                .and_then(Value::as_str)
                .or_else(|| map.get("kind").and_then(Value::as_str))
                .or_else(|| {
                    map.get("agent_session")
                        .and_then(Value::as_object)
                        .and_then(|session| session.get("agent"))
                        .and_then(Value::as_str)
                });
            if let (Some(pane_id), Some(kind)) = (pane_id, kind) {
                if let Some(harness) = Harness::from_agent_name(kind) {
                    let tokens: BTreeMap<String, String> = map
                        .get("tokens")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|tokens| tokens.iter())
                        .filter_map(|(name, value)| {
                            value
                                .as_str()
                                .map(|value| (name.clone(), value.to_string()))
                        })
                        .collect();
                    let topic = tokens.get("quota_topic").cloned().unwrap_or_default();
                    let session_summary = tokens.get("quota_session").cloned().unwrap_or_default();
                    let session =
                        map.get("agent_session")
                            .and_then(Value::as_object)
                            .and_then(|session| {
                                session.get("value").and_then(Value::as_str).map(|value| {
                                    AgentSession {
                                        kind: session
                                            .get("kind")
                                            .and_then(Value::as_str)
                                            .map(str::to_string),
                                        value: value.to_string(),
                                    }
                                })
                            });
                    let workspace_id = map
                        .get("workspace_id")
                        .or_else(|| map.get("workspaceId"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| workspace_id_from_pane_id(pane_id))
                        .unwrap_or_default();
                    let status = map
                        .get("agent_status")
                        .or_else(|| map.get("agentStatus"))
                        .or_else(|| map.get("status"))
                        .or_else(|| map.get("state"))
                        .and_then(Value::as_str)
                        .map(AgentStatus::parse)
                        .unwrap_or_default();
                    let focused = map.get("focused").and_then(Value::as_bool).unwrap_or(false);
                    let state_labels: BTreeMap<String, String> = map
                        .get("state_labels")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|labels| labels.iter())
                        .filter_map(|(status, text)| {
                            text.as_str().map(|text| (status.clone(), text.to_string()))
                        })
                        .collect();
                    panes.push(AgentPane {
                        pane_id: pane_id.to_string(),
                        workspace_id,
                        harness,
                        session,
                        session_summary,
                        // Preserve the last published topic during quota-only
                        // refreshes. Agent events refresh it from pane output.
                        topic,
                        tokens,
                        status,
                        focused,
                        state_labels,
                    });
                }
            }
            for child in map.values() {
                collect_agent_panes(child, panes);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_agent_panes(child, panes);
            }
        }
        _ => {}
    }
}

fn working_providers_from(value: &Value) -> Vec<Provider> {
    let mut providers = Vec::new();
    collect_working_providers(value, &mut providers, &mut Vec::new());
    providers.sort_by_key(|provider| {
        Provider::ALL
            .iter()
            .position(|candidate| candidate == provider)
    });
    providers.dedup();
    providers
}

fn collect_working_providers(
    value: &Value,
    providers: &mut Vec<Provider>,
    pane_ids: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            let kind = map
                .get("agent")
                .and_then(Value::as_str)
                .or_else(|| map.get("kind").and_then(Value::as_str))
                .or_else(|| {
                    map.get("agent_session")
                        .and_then(Value::as_object)
                        .and_then(|session| session.get("agent"))
                        .and_then(Value::as_str)
                });
            let status = map
                .get("agent_status")
                .or_else(|| map.get("agentStatus"))
                .or_else(|| map.get("status"))
                .or_else(|| map.get("state"))
                .and_then(Value::as_str);
            if let (Some(kind), Some(status)) = (kind, status) {
                if status.eq_ignore_ascii_case("working") {
                    if Harness::from_agent_name(kind).is_some() {
                        if let Some(pane_id) = map
                            .get("pane_id")
                            .or_else(|| map.get("paneId"))
                            .and_then(Value::as_str)
                        {
                            pane_ids.push(pane_id.to_string());
                        }
                    }
                    if let Some(provider) = Harness::billing_for_agent(kind) {
                        providers.push(provider);
                    }
                }
            }
            for child in map.values() {
                collect_working_providers(child, providers, pane_ids);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_working_providers(child, providers, pane_ids);
            }
        }
        _ => {}
    }
}

pub fn publish_pane_tokens(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, false)
}

/// Watcher refreshes may need to clear a stale icon while its pane is scrolled.
/// The inner publisher still sends only the icon twins in that case; quota and
/// topic writes remain deferred until the pane is visible.
pub fn publish_pane_tokens_with_scrolled_icons(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, true)
}

/// Sidebar icon colour must update on focus even if the terminal is scrolled —
/// the scroll guard exists to protect reading scrollback during quota refreshes,
/// not to leave a stale teal glyph after mark-seen.
pub fn publish_status_icons(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, true)
}

/// Focus and watcher reconciliation change only the three icon colour tokens.
/// They must not turn an icon acknowledgement into a quota or group refresh.
pub fn publish_icon_tokens(panes: &[AgentPane], sequence: u64) -> Result<()> {
    if panes.is_empty() {
        return Ok(());
    }
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let inventory = match list_agent_panes() {
        Ok(all) if !all.is_empty() => all,
        _ => panes.to_vec(),
    };
    let order = installed_agent_order();
    // No labels here: every pane sits in the group its `al_grp` names. The
    // workspace numbers are read only when a custom group can span Spaces.
    let rules = crate::groups::Groups::load();
    let ranks = if order.custom_groups() && !rules.is_empty() {
        list_workspaces()
            .map(|(_, ranks)| ranks)
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let grouping = Grouping::new(order, rules, &[], None, ranks);
    let group_heads = space_group_heads(order, &grouping, &inventory, panes, &[]);
    let look = Look::installed(order, &grouping, &inventory, panes, &[]);
    let mut reported = 0;
    let mut failed = Vec::new();
    for pane in panes {
        let mut desired = pane.tokens.clone();
        apply_group_and_icon(
            &mut desired,
            pane,
            &grouping,
            &group_heads,
            &BTreeMap::new(),
            &look,
        );
        if icon_tokens_match(&pane.tokens, &desired) {
            continue;
        }
        reported += 1;
        if !report_icon_metadata(&executable, pane, &desired, sequence)? {
            failed.push(pane.pane_id.clone());
        }
    }
    if reported > 0 && failed.len() == reported {
        anyhow::bail!(
            "Herdr icon report failed for every pane: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn publish_pane_tokens_inner(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
    allow_while_scrolled: bool,
) -> Result<()> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let (workspace_labels, workspace_ranks) = list_workspaces().unwrap_or_default();
    // The Space name a seat row shows (`$al_space`) is the short form; the
    // group header keeps the full label.
    let space_names = short_space_names(&workspace_labels);
    // Event/focus publish one pane. Head selection and sibling clears need the
    // full Space membership — otherwise a lone pane preserves a stale
    // `$quota_group` and the Space name prints twice (radar writes `group:null`
    // on every non-head every frame).
    let inventory = match list_agent_panes() {
        Ok(all) if !all.is_empty() => all,
        _ => panes.to_vec(),
    };
    let order = installed_agent_order();
    // One metadata-only read per pass. A failed read preserves whatever
    // identity (and group) the panes already show rather than clearing it.
    let pane_labels = if panes.is_empty() {
        None
    } else {
        list_pane_labels().ok()
    };
    let grouping = Grouping::new(
        order,
        crate::groups::Groups::load(),
        panes,
        pane_labels.as_ref(),
        workspace_ranks,
    );
    let mut group_heads = space_group_heads(order, &grouping, &inventory, panes, tokens);
    let mut look = Look::installed(order, &grouping, &inventory, panes, tokens);
    // Only a logo layout needs the agent processes' channel.
    let channels = look.logos.shows_logos().then(crate::channel::scan);
    let desired_for = |pane: &AgentPane,
                       pane_tokens: &PaneTokens,
                       group_heads: &BTreeMap<String, String>,
                       look: &Look| {
        let topic = display_topic(pane);
        let mut desired = match &pane_tokens.quota {
            PaneQuotaUpdate::Replace(values) => desired_tokens(values, &topic, row.shape),
            PaneQuotaUpdate::Clear => desired_cleared_quota(pane),
            PaneQuotaUpdate::Preserve => pane.tokens.clone(),
        };
        if let Some(identity) = &pane_tokens.identity {
            apply_identity(&mut desired, identity, row.shape.content_width);
        }
        if let Some(context) = &pane_tokens.context {
            apply_context(&mut desired, context, sequence / 1_000, row);
        }
        fold_cache_row(&mut desired, row);
        apply_seat_identity(
            &mut desired,
            pane,
            pane_labels.as_ref(),
            order,
            &space_names,
        );
        apply_logos(&mut desired, pane, look, channels.as_ref());
        apply_grouping(&mut desired, pane, &grouping, &space_names);
        apply_group_and_icon(
            &mut desired,
            pane,
            &grouping,
            group_heads,
            &workspace_labels,
            look,
        );
        let headroom = desired
            .get(HEADROOM_TOKEN)
            .and_then(|value| value.parse().ok())
            .unwrap_or(u8::MAX);
        desired.insert(SORT_TOKEN.to_string(), sort_key(headroom, &pane.pane_id));
        desired
    };
    let with_tokens = panes
        .iter()
        .filter_map(|pane| {
            tokens
                .iter()
                .find(|tokens| tokens.pane_id == pane.pane_id)
                .map(|pane_tokens| (pane, pane_tokens))
        })
        .collect::<Vec<_>>();
    // Herdr versions that repaint metadata can snap a terminal viewport back
    // to the bottom, so a pane the user is reading scrollback in is not
    // written this pass — and keeps its old `al_sort`. The head must then be
    // chosen by the key the view will actually see, or the Space shows no
    // head (or a bare pane above it) until the user scrolls back.
    let mut held = BTreeSet::new();
    let mut checked = BTreeSet::new();
    for (pane, pane_tokens) in &with_tokens {
        let desired = desired_for(pane, pane_tokens, &group_heads, &look);
        let changed =
            !metadata_matches(&pane.tokens, &desired) || label_change(pane) != LabelChange::Keep;
        if !changed {
            continue;
        }
        checked.insert(pane.pane_id.clone());
        if pane_is_scrolled(&executable, &pane.pane_id) {
            held.insert(pane.pane_id.clone());
        }
    }
    if !held.is_empty() {
        let settled = panes
            .iter()
            .filter(|pane| !held.contains(&pane.pane_id))
            .cloned()
            .collect::<Vec<_>>();
        group_heads = space_group_heads(order, &grouping, &inventory, &settled, tokens);
        look.tails = space_tails(order, look.group, &grouping, &inventory, &settled, tokens);
    }
    let mut reported = 0usize;
    let mut failed = Vec::new();
    for (pane, pane_tokens) in &with_tokens {
        let desired = desired_for(pane, pane_tokens, &group_heads, &look);
        let labels = label_change(pane);
        if metadata_matches(&pane.tokens, &desired) && labels == LabelChange::Keep {
            continue;
        }
        // A pane only the re-chosen heads changed was not looked at yet.
        let scrolled = held.contains(&pane.pane_id)
            || (!checked.contains(&pane.pane_id) && pane_is_scrolled(&executable, &pane.pane_id));
        // Icon-only sync may proceed while scrolled — see
        // publish_status_icons.
        if scrolled {
            if !allow_while_scrolled || icon_tokens_match(&pane.tokens, &desired) {
                continue;
            }
            reported += 1;
            if !report_icon_metadata(&executable, pane, &desired, sequence)? {
                failed.push(pane.pane_id.clone());
            }
            continue;
        }
        reported += 1;
        if !report_pane_metadata(&executable, pane, &desired, sequence, labels, look.rows)? {
            failed.push(pane.pane_id.clone());
        }
    }
    let (synced, sync_failed) = sync_sibling_group_headers(
        &executable,
        &inventory,
        panes,
        &grouping,
        &group_heads,
        &workspace_labels,
        &look,
        sequence,
    )?;
    reported += synced;
    failed.extend(sync_failed);
    // A pane can exit between `agent list` and this report, and the exit event
    // itself triggers a publish. One stale pane id must not stop the panes
    // that are still alive from being updated.
    if reported > 0 && failed.len() == reported {
        anyhow::bail!(
            "Herdr metadata report failed for every pane: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn icon_tokens_match(
    current: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> bool {
    ICON_REPORT_NAMES
        .into_iter()
        .all(|name| current.get(name) == desired.get(name))
}

fn report_icon_metadata(
    executable: &std::ffi::OsStr,
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    sequence: u64,
) -> Result<bool> {
    let mut command = Command::new(executable);
    command
        .args([
            "pane",
            "report-metadata",
            &pane.pane_id,
            "--source",
            "herdr-agent-quota",
        ])
        .args(["--seq", &sequence.to_string(), "--ttl-ms", METADATA_TTL_MS]);
    for name in ICON_REPORT_NAMES {
        if let Some(value) = desired.get(name) {
            command.args(["--token", &format!("{name}={value}")]);
        } else {
            command.args(["--clear-token", name]);
        }
    }
    let output = command.output().context("report icon metadata to Herdr")?;
    Ok(output.status.success())
}

fn report_pane_metadata(
    executable: &std::ffi::OsStr,
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    sequence: u64,
    labels: LabelChange,
    rows: crate::cli::SidebarRows,
) -> Result<bool> {
    let mut command = Command::new(executable);
    command
        .args([
            "pane",
            "report-metadata",
            &pane.pane_id,
            "--source",
            "herdr-agent-quota",
        ])
        .args(["--seq", &sequence.to_string()])
        .args(["--ttl-ms", METADATA_TTL_MS]);
    for name in metadata_report_names(pane, desired, rows) {
        if let Some(value) = desired.get(name) {
            command.args(["--token", &format!("{name}={value}")]);
        } else {
            command.args(["--clear-token", name]);
        }
    }
    command.args(state_label_args(labels));
    let output = command.output().context("report quota metadata to Herdr")?;
    Ok(output.status.success())
}

/// Arguments that carry a [`LabelChange`]. Labels ride on the quota report
/// they accompany and never cost a separate write; they are not tokens, so
/// they do not compete for Herdr's sixteen-token budget either.
fn state_label_args(labels: LabelChange) -> Vec<String> {
    match labels {
        LabelChange::Keep => Vec::new(),
        LabelChange::Write => ATTENTION_STATE_LABELS
            .into_iter()
            .flat_map(|(status, text)| ["--state-label".to_string(), format!("{status}={text}")])
            .collect(),
    }
}

/// Clear or set `$quota_group` on siblings in the same Space that this pass
/// did not otherwise touch. Without this, a one-pane event leaves the old
/// head's header in place after headroom moves the title to another pane.
#[allow(clippy::too_many_arguments)]
fn sync_sibling_group_headers(
    executable: &std::ffi::OsStr,
    inventory: &[AgentPane],
    published: &[AgentPane],
    grouping: &Grouping,
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
    look: &Look,
    sequence: u64,
) -> Result<(usize, Vec<String>)> {
    let mut failed = Vec::new();
    let published_ids = published
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    // Siblings of every group a published pane left or joined: the Space it
    // is in and, for a custom-group member, that group.
    let touched = published
        .iter()
        .flat_map(|pane| [grouping.gid(pane), pane.workspace_id.clone()])
        .filter(|gid| !gid.is_empty())
        .collect::<BTreeSet<_>>();
    let mut reported = 0usize;
    for sibling in inventory {
        if published_ids.contains(sibling.pane_id.as_str()) {
            continue;
        }
        let gid = grouping.gid(sibling);
        if !touched.contains(&gid) && !touched.contains(&sibling.workspace_id) {
            continue;
        }
        let mut wanted = BTreeMap::new();
        let header = group_label_for(sibling, grouping, group_heads, workspace_labels)
            .map(|name| look.header(&gid, &name));
        apply_header(&mut wanted, &sibling.pane_id, header, look);
        let unchanged = ["quota_group", GAP_TOKEN].into_iter().all(|name| {
            wanted.get(name) == sibling.tokens.get(name).filter(|value| !value.is_empty())
        });
        if unchanged {
            continue;
        }
        if pane_is_scrolled(executable, &sibling.pane_id) {
            continue;
        }
        reported += 1;
        let mut command = Command::new(executable);
        command
            .args([
                "pane",
                "report-metadata",
                &sibling.pane_id,
                "--source",
                "herdr-agent-quota",
            ])
            .args(["--seq", &sequence.to_string()])
            .args(["--ttl-ms", METADATA_TTL_MS]);
        for name in ["quota_group", GAP_TOKEN] {
            match wanted.get(name) {
                Some(value) => command.args(["--token", &format!("{name}={value}")]),
                None => command.args(["--clear-token", name]),
            };
        }
        let output = command.output().context("report group header to Herdr")?;
        if !output.status.success() {
            failed.push(sibling.pane_id.clone());
        }
    }
    Ok((reported, failed))
}

fn pane_is_scrolled(executable: &std::ffi::OsStr, pane_id: &str) -> bool {
    let Ok(output) = Command::new(executable)
        .args(["pane", "get", pane_id])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/pane/scroll/offset_from_bottom")
                .and_then(Value::as_u64)
        })
        .is_some_and(|offset| offset > 0)
}

/// `w1:p9` → `w1`. Used when an older inventory omits `workspace_id`.
fn workspace_id_from_pane_id(pane_id: &str) -> Option<String> {
    let (workspace, _) = pane_id.split_once(':')?;
    (!workspace.is_empty()).then(|| workspace.to_string())
}

/// Workspace id → label from one `herdr workspace list` call.
/// Role, model short name and (ungrouped) Space for one seat.
///
/// `pane_labels` is `None` when the label read failed: then the identity
/// already on the pane is kept, never cleared. A label that is present but
/// off-spec clears it — that is "not reported", not a failed read.
fn apply_seat_identity(
    desired: &mut BTreeMap<String, String>,
    pane: &AgentPane,
    pane_labels: Option<&BTreeMap<String, String>>,
    order: crate::cli::AgentOrder,
    workspace_labels: &BTreeMap<String, String>,
) {
    match pane_labels {
        Some(labels) => {
            let identity = labels
                .get(&pane.pane_id)
                .map(|label| crate::identity::from_label(label))
                .unwrap_or_default();
            if identity.role.is_empty() {
                desired.remove(ROLE_TOKEN);
            } else {
                desired.insert(ROLE_TOKEN.to_string(), identity.role);
            }
        }
        None => {
            match pane.tokens.get(ROLE_TOKEN) {
                Some(value) => desired.insert(ROLE_TOKEN.to_string(), value.clone()),
                None => desired.remove(ROLE_TOKEN),
            };
        }
    }
    // The model name comes from the same evidence as the model mark: the
    // model this pane's own session reported. The label's model segment is a
    // naming snapshot (a Codex coordinator writes `codex-home` there), so a
    // pane without that evidence shows no model name (owner 0927).
    match session_model(pane, desired).map(crate::look::short_model_name) {
        Some(name) if !name.is_empty() => desired.insert(MODEL_TOKEN.to_string(), name),
        _ => desired.remove(MODEL_TOKEN),
    };
    let space = (!order.groups_by_space())
        .then(|| workspace_labels.get(&pane.workspace_id))
        .flatten()
        .filter(|label| !label.is_empty());
    match space {
        Some(label) => desired.insert(SPACE_TOKEN.to_string(), label.clone()),
        None => desired.remove(SPACE_TOKEN),
    };
}

/// The model a pane's own agent session reported (`quota_model`, per session
/// from the Claude Code statusLine). A session-less pane can still hold a
/// `quota_model` an older build borrowed from another seat, so it counts as
/// no evidence.
fn session_model<'a>(pane: &AgentPane, desired: &'a BTreeMap<String, String>) -> Option<&'a str> {
    pane.session.as_ref()?;
    desired
        .get("quota_model")
        .map(String::as_str)
        .filter(|model| !model.trim().is_empty())
}

/// Pane id → label from one `herdr pane list` call. Metadata only: this
/// reads no terminal output and repaints nothing.
pub(crate) fn list_pane_labels() -> Result<BTreeMap<String, String>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["pane", "list"])
        .output()
        .context("list Herdr panes")?;
    if !output.status.success() {
        anyhow::bail!("Herdr pane list failed with {}", output.status);
    }
    let value: Value = serde_json::from_slice(&output.stdout).context("parse pane list")?;
    let mut labels = BTreeMap::new();
    collect_pane_labels(&value, &mut labels);
    Ok(labels)
}

fn collect_pane_labels(value: &Value, labels: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            let id = map.get("pane_id").and_then(Value::as_str);
            let label = map.get("label").and_then(Value::as_str);
            if let (Some(id), Some(label)) = (id, label) {
                labels.insert(id.to_string(), label.to_string());
            }
            for child in map.values() {
                collect_pane_labels(child, labels);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_pane_labels(child, labels);
            }
        }
        _ => {}
    }
}

/// Workspace id → label and workspace id → number (Herdr's
/// `workspace_order`), from one `herdr workspace list` call.
fn list_workspaces() -> Result<(BTreeMap<String, String>, BTreeMap<String, u64>)> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["workspace", "list"])
        .output()
        .context("list Herdr workspaces")?;
    if !output.status.success() {
        anyhow::bail!("Herdr workspace list failed with {}", output.status);
    }
    let value: Value = serde_json::from_slice(&output.stdout).context("parse workspace list")?;
    let mut labels = BTreeMap::new();
    collect_workspace_labels(&value, &mut labels);
    let mut ranks = BTreeMap::new();
    collect_workspace_ranks(&value, &mut ranks);
    Ok((labels, ranks))
}

fn collect_workspace_ranks(value: &Value, ranks: &mut BTreeMap<String, u64>) {
    match value {
        Value::Object(map) => {
            let id = map
                .get("workspace_id")
                .or_else(|| map.get("workspaceId"))
                .and_then(Value::as_str);
            if let (Some(id), Some(number)) = (id, map.get("number").and_then(Value::as_u64)) {
                ranks.insert(id.to_string(), number);
            }
            for child in map.values() {
                collect_workspace_ranks(child, ranks);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_workspace_ranks(child, ranks);
            }
        }
        _ => {}
    }
}

fn collect_workspace_labels(value: &Value, labels: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            let id = map
                .get("workspace_id")
                .or_else(|| map.get("workspaceId"))
                .and_then(Value::as_str);
            let label = map.get("label").and_then(Value::as_str);
            if let (Some(id), Some(label)) = (id, label) {
                labels.insert(id.to_string(), label.to_string());
            }
            for child in map.values() {
                collect_workspace_labels(child, labels);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_workspace_labels(child, labels);
            }
        }
        _ => {}
    }
}

/// Which pane carries the group header for each workspace.
///
/// Under quota order the view sorts by `workspace_order` then headroom, so the
/// tightest pane sits at the top of its Space — that is where the header must
/// land. `inventory` is the full agent list; `publishing` / `tokens` overlay
/// headroom for panes this pass is about to write so a forced refresh that
/// moves the title does not leave the old head labelled.
fn group_head_pane_ids(
    grouping: &Grouping,
    inventory: &[AgentPane],
    publishing: &[AgentPane],
    tokens: &[PaneTokens],
) -> BTreeMap<String, String> {
    space_ends(grouping, inventory, publishing, tokens)
        .into_iter()
        .map(|(workspace, (head, _))| (workspace, head))
        .collect()
}

/// The last pane of each Space, which carries the gap row — only for a
/// grouped view in a gapped style.
fn space_tails(
    order: crate::cli::AgentOrder,
    group: crate::look::GroupStyle,
    grouping: &Grouping,
    inventory: &[AgentPane],
    publishing: &[AgentPane],
    tokens: &[PaneTokens],
) -> BTreeSet<String> {
    if !(order.groups_by_space() && group.gaps()) {
        return BTreeSet::new();
    }
    space_ends(grouping, inventory, publishing, tokens)
        .into_values()
        .map(|(_, tail)| tail)
        .collect()
}

/// Sorts after every [`sort_key`]: Herdr lists a pane without the token last.
const UNSORTED: char = '\u{10ffff}';

/// Per workspace, the first and last pane by [`sort_key`] — the order the
/// grouped view lists them in. `inventory` is the full agent list;
/// `publishing` / `tokens` overlay headroom for panes this pass is about to
/// write, so a forced refresh that moves the title does not leave the old
/// head labelled.
fn space_ends(
    grouping: &Grouping,
    inventory: &[AgentPane],
    publishing: &[AgentPane],
    tokens: &[PaneTokens],
) -> BTreeMap<String, (String, String)> {
    let publishing_ids = publishing
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut ends: BTreeMap<String, (String, String, String, String)> = BTreeMap::new();
    let mut consider = |pane: &AgentPane, key: String| {
        let gid = grouping.gid(pane);
        if gid.is_empty() {
            return;
        }
        let id = pane.pane_id.clone();
        match ends.get_mut(&gid) {
            Some((first_key, first, last_key, last)) => {
                if key < *first_key {
                    *first_key = key.clone();
                    *first = id.clone();
                }
                if key > *last_key {
                    *last_key = key;
                    *last = id;
                }
            }
            None => {
                ends.insert(gid, (key.clone(), id.clone(), key, id));
            }
        }
    };
    for pane in inventory {
        // A pane this pass does not write sits where its own `al_sort` puts
        // it — even if its headroom token lagged a report — and Herdr lists a
        // pane without the token last.
        let headroom = publishing_ids
            .contains(pane.pane_id.as_str())
            .then(|| published_headroom(pane, tokens));
        consider(pane, grouping.place(pane, headroom));
    }
    // A pane present only in this pass (inventory read failed) still counts,
    // so its Space is not left without a label.
    for pane in publishing {
        if !inventory.iter().any(|known| known.pane_id == pane.pane_id) {
            consider(
                pane,
                grouping.place(pane, Some(published_headroom(pane, tokens))),
            );
        }
    }
    ends.into_iter()
        .map(|(workspace, (_, first, _, last))| (workspace, (first, last)))
        .collect()
}

/// The headroom this pass writes for a pane — the same value its `al_sort`
/// is built from. A replacement without headroom and a clear both drop the
/// token (sorted last); only a preserved pane keeps what it shows.
fn published_headroom(pane: &AgentPane, tokens: &[PaneTokens]) -> u8 {
    let current = || {
        pane.tokens
            .get(HEADROOM_TOKEN)
            .and_then(|value| value.parse().ok())
            .unwrap_or(u8::MAX)
    };
    match tokens
        .iter()
        .find(|tokens| tokens.pane_id == pane.pane_id)
        .map(|tokens| &tokens.quota)
    {
        Some(PaneQuotaUpdate::Replace(values)) => values.quota_headroom.unwrap_or(u8::MAX),
        Some(PaneQuotaUpdate::Clear) => u8::MAX,
        Some(PaneQuotaUpdate::Preserve) | None => current(),
    }
}

fn group_label_for(
    pane: &AgentPane,
    grouping: &Grouping,
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
) -> Option<String> {
    let gid = grouping.gid(pane);
    if gid.is_empty() {
        return None;
    }
    if group_heads
        .get(&gid)
        .is_none_or(|head| head != &pane.pane_id)
    {
        return None;
    }
    grouping.name(&gid, workspace_labels)
}

/// Herdr hang-indents a head pane's rows under `$quota_group`. Member panes
/// collapse an empty group, so their identity is row 1 and needs the same
/// offset baked into the first plugin cell — same as herdr-radar
/// `group_indent = 2`.
///
/// The indent must ride on the logo token, not a stand-alone `$quota_pad`:
/// Herdr inserts ` · ` between adjacent non-empty tokens, so a pad cell drew
/// a leading middle-dot before the logo. ZWSP + spaces survive trim only when
/// a non-whitespace glyph follows in the same value.
const GROUP_MEMBER_INDENT: &str = "\u{200b}  ";
/// Always reported together so a lagging inventory cannot leave a stale
/// colour twin on screen after working→done or done→idle.
const ICON_TOKEN_NAMES: [&str; 3] = ["quota_icon", "quota_icon_working", "quota_icon_done"];
/// What an icon-only report carries: the three colour twins and the
/// attention rank that sorts by that colour.
const ICON_REPORT_NAMES: [&str; 4] = [
    "quota_icon",
    "quota_icon_working",
    "quota_icon_done",
    ATTENTION_TOKEN,
];

/// What Herdr's built-in `state_text` prints under the attention order: the
/// DESIGN-r3 head row's action state, first thing on the identity row.
///
/// Labels, not tokens, on purpose. Herdr resolves the label from the pane's
/// live status when it renders, so a status change repaints the right text in
/// Herdr's own per-status colour without any metadata write — the label set
/// is written once per pane and read back from `agent list` afterwards. A
/// report that names any label replaces the whole set, so all five are always
/// sent together. `unknown` gets an explicit "not reported" rather than being
/// left to Herdr's default, which prints `idle` for it.
///
/// There is no `[待拍N]`: no per-pane source for pending owner decisions
/// exists, and a count that cannot be read is not shown.
pub(crate) const ATTENTION_STATE_LABELS: [(&str, &str); 5] = [
    ("blocked", "[卡住]"),
    ("done", "[已回]"),
    ("working", "[工作]"),
    ("idle", "[空闲]"),
    ("unknown", "[未报]"),
];

/// Does the pane already carry exactly the label set this plugin writes?
fn state_labels_current(labels: &BTreeMap<String, String>) -> bool {
    labels.len() == ATTENTION_STATE_LABELS.len()
        && ATTENTION_STATE_LABELS
            .into_iter()
            .all(|(status, text)| labels.get(status).map(String::as_str) == Some(text))
}

/// The label change a publish owes this pane, if any.
///
/// The labels are wanted in every order: the identity rows lead with
/// `state_text` in both, so the rows need no rewrite when the order toggles.
/// Herdr cannot hide a label cleanly (an empty one is rejected, a zero-width
/// one leaves a stray ` · ` separator), so grouped panels show it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelChange {
    Keep,
    Write,
}

fn label_change(pane: &AgentPane) -> LabelChange {
    if state_labels_current(&pane.state_labels) {
        LabelChange::Keep
    } else {
        LabelChange::Write
    }
}

/// The installed Agent order, read the way configure stored it.
///
/// Only a Herdr-launched process has a plugin state directory; anything else
/// (a unit test, a bare CLI run) gets the default rather than whatever an
/// unrelated installation left in the user's data directory.
fn installed_agent_order() -> crate::cli::AgentOrder {
    let cache = std::env::var_os("HERDR_PLUGIN_STATE_DIR").map(crate::cache::CacheStore::new);
    crate::configure::resolved_agent_order(None, cache.as_ref())
}

/// Space-head selection, or none when the panel is not grouped by Space.
///
/// Under the attention order a Space's panes are scattered, so a header on
/// its tightest pane would sit mid-list and name the wrong neighbours. An
/// empty map makes every pane a non-head: the header token is cleared and no
/// member indent is added.
fn space_group_heads(
    order: crate::cli::AgentOrder,
    grouping: &Grouping,
    inventory: &[AgentPane],
    publishing: &[AgentPane],
    tokens: &[PaneTokens],
) -> BTreeMap<String, String> {
    if order.groups_by_space() {
        group_head_pane_ids(grouping, inventory, publishing, tokens)
    } else {
        BTreeMap::new()
    }
}

/// Vendor mark always; group header only on the Space head pane.
///
/// Exactly one of `$quota_icon` / `_working` / `_done` is published so the
/// brand glyph itself carries Herdr's status colour (no `state_icon` ring).
/// Members prefix the logo with [`GROUP_MEMBER_INDENT`]. Heads publish the
/// bare glyph — Herdr already hang-indents their continuation rows. Stale
/// `$quota_pad` from older builds is cleared.
fn apply_group_and_icon(
    desired: &mut BTreeMap<String, String>,
    pane: &AgentPane,
    grouping: &Grouping,
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
    look: &Look,
) {
    let style = look.style;
    // The model segment this pass is about to publish, else the one already
    // on the pane: an icon-only pass must not swap a whale back to a star.
    let model = desired
        .get(MODEL_TOKEN)
        .or_else(|| pane.tokens.get(MODEL_TOKEN))
        .map(String::as_str)
        .unwrap_or_default();
    // With logos on, the first mark is the harness itself — a Claude Code
    // seat on DeepSeek is still the crab; the whale is its model mark (owner
    // 0927). The logo-less rows keep round two's whale.
    let glyph = if look.logos.shows_logos() {
        crate::icons::for_harness_styled(pane.harness, style)
    } else {
        crate::icons::for_pane(pane.harness, model, style)
    };
    let gid = grouping.gid(pane);
    let member = !gid.is_empty()
        && group_heads
            .get(&gid)
            .is_some_and(|head| head != &pane.pane_id);
    let mark = if member {
        format!("{GROUP_MEMBER_INDENT}{glyph}")
    } else {
        glyph.to_string()
    };
    let status = pane.icon_status();
    let active = status.icon_token();
    for token in ICON_TOKEN_NAMES {
        if token == active {
            desired.insert(token.to_string(), mark.clone());
        } else {
            desired.remove(token);
        }
    }
    desired.insert(
        ATTENTION_TOKEN.to_string(),
        status.attention_rank().to_string(),
    );
    desired.remove("quota_pad");
    // Never preserve a previous header: non-heads must omit the token so the
    // report clears it. Blind preserve is what left `ifs` on two panes.
    let header = group_label_for(pane, grouping, group_heads, workspace_labels)
        .map(|name| look.header(&gid, &name));
    apply_header(desired, &pane.pane_id, header, look);
}

/// The Space name a seat row carries: the label's first `-` segment
/// (`siflow-inference-lab` → `siflow`), so a narrow sidebar keeps the Tab
/// name (owner 0928, option 乙). Two Spaces that would shorten to the same
/// name both keep their full label.
fn short_space_names(labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let short = |label: &str| {
        label
            .split('-')
            .next()
            .filter(|first| !first.is_empty())
            .unwrap_or(label)
            .to_string()
    };
    let mut uses: BTreeMap<String, usize> = BTreeMap::new();
    for label in labels.values() {
        *uses.entry(short(label)).or_default() += 1;
    }
    labels
        .iter()
        .map(|(id, label)| {
            let name = short(label);
            let name = if uses.get(&name) == Some(&1) {
                name
            } else {
                label.clone()
            };
            (id.clone(), name)
        })
        .collect()
}

/// `al_grp` and `al_role_rank` on every seat, and the Space name on a
/// custom-group member — its header names the group, not the line it is in.
fn apply_grouping(
    desired: &mut BTreeMap<String, String>,
    pane: &AgentPane,
    grouping: &Grouping,
    workspace_labels: &BTreeMap<String, String>,
) {
    match grouping
        .group_value(pane)
        .or_else(|| pane.tokens.get(GROUP_TOKEN).cloned())
    {
        Some(value) => desired.insert(GROUP_TOKEN.to_string(), value),
        None => desired.remove(GROUP_TOKEN),
    };
    let role = desired.get(ROLE_TOKEN).map(String::as_str).unwrap_or("");
    desired.insert(ROLE_RANK_TOKEN.to_string(), role_rank(role).to_string());
    if grouping.custom_group(pane).is_some() {
        if let Some(label) = workspace_labels
            .get(&pane.workspace_id)
            .filter(|label| !label.is_empty())
        {
            desired.insert(SPACE_TOKEN.to_string(), label.clone());
        }
    }
}

/// `$quota_group` on a Space head and, for gapped styles, the blank row at
/// the end of a Space's last pane.
fn apply_header(
    desired: &mut BTreeMap<String, String>,
    pane_id: &str,
    header: Option<String>,
    look: &Look,
) {
    match header {
        Some(header) => desired.insert("quota_group".to_string(), header),
        None => desired.remove("quota_group"),
    };
    if look.tails.contains(pane_id) {
        desired.insert(GAP_TOKEN.to_string(), crate::look::GAP.to_string());
    } else {
        desired.remove(GAP_TOKEN);
    }
}

/// Channel and model-family marks, only when a logo layout is on.
fn apply_logos(
    desired: &mut BTreeMap<String, String>,
    pane: &AgentPane,
    look: &Look,
    channels: Option<&BTreeMap<String, crate::channel::ProcessChannel>>,
) {
    if !look.logos.shows_logos() {
        desired.remove(CHANNEL_TOKEN);
        desired.remove(MODEL_MARK_TOKEN);
        return;
    }
    let process = channels.and_then(|channels| channels.get(&pane.pane_id));
    match crate::channel::classify(pane.harness, process)
        .and_then(|channel| crate::look::channel_mark(channel, pane.harness, look.style))
    {
        Some(mark) => desired.insert(CHANNEL_TOKEN.to_string(), mark.to_string()),
        None => desired.remove(CHANNEL_TOKEN),
    };
    // The model the pane's own session reported (`quota_model`, per session
    // from the Claude Code statusLine), never the label's model segment —
    // that is a naming snapshot, and a Codex coordinator writes `codex-home`
    // there. Only a pane with its own agent session counts: a session-less
    // pane can still hold a `quota_model` an older build borrowed (0927: two
    // such panes showed another seat's "Fable 5.1"). Nothing reported, no
    // mark.
    let model = session_model(pane, desired).unwrap_or_default();
    match crate::look::model_family(model) {
        Some(family) => desired.insert(
            MODEL_MARK_TOKEN.to_string(),
            crate::look::model_mark(family, look.style).to_string(),
        ),
        None => desired.remove(MODEL_MARK_TOKEN),
    };
}

fn desired_tokens(
    values: &MetadataTokens,
    topic: &str,
    shape: SidebarShape,
) -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::new();
    insert_optional_token(&mut tokens, "quota_provider", &values.quota_provider);
    tokens.insert(
        "quota_provider_model".to_string(),
        values.quota_provider_model.clone(),
    );
    insert_optional_token(&mut tokens, "quota_model", &values.quota_model);
    insert_context_token(
        &mut tokens,
        &values.quota_context,
        values.quota_context_severity,
        shape,
    );
    insert_optional_token(&mut tokens, "quota_cache", &values.quota_cache);
    insert_optional_token(&mut tokens, "quota_cache_ttl", &values.quota_cache_ttl);
    insert_optional_token(&mut tokens, "quota_cache_state", &values.quota_cache_state);
    let week_base = week_style_base(&values.quota_5h);
    insert_severity_token(
        &mut tokens,
        "quota_5h",
        &values.quota_5h,
        values.quota_5h_severity,
    );
    insert_severity_token(
        &mut tokens,
        week_base,
        &values.quota_week,
        values.quota_week_severity,
    );
    insert_severity_token(
        &mut tokens,
        "quota_month",
        &values.quota_month,
        values.quota_month_severity,
    );
    insert_optional_token(&mut tokens, "quota_topic", topic);
    if let Some(error) = &values.quota_error {
        tokens.insert("quota_error".to_string(), error.clone());
    }
    if let Some(headroom) = values.quota_headroom {
        tokens.insert(HEADROOM_TOKEN.to_string(), format!("{headroom:03}"));
    }
    tokens
}

fn display_topic(pane: &AgentPane) -> String {
    let topic = pane.topic.trim();
    if topic.is_empty() || is_status_line(topic) {
        return truncate_topic(&pane.session_summary);
    }
    truncate_topic(topic)
}

pub(crate) fn plugin_quota_present(tokens: &BTreeMap<String, String>) -> bool {
    METADATA_TOKEN_NAMES
        .into_iter()
        .chain(OBSOLETE_METADATA_TOKEN_NAMES)
        .chain(LEGACY_METADATA_TOKEN_NAMES)
        .filter(|name| *name != "quota_topic")
        .any(|name| tokens.contains_key(name))
}

fn desired_cleared_quota(pane: &AgentPane) -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::new();
    let topic = display_topic(pane);
    if !topic.is_empty() {
        tokens.insert("quota_topic".to_string(), topic);
    }
    tokens
}

fn apply_identity(
    tokens: &mut BTreeMap<String, String>,
    identity: &PaneIdentity,
    content_width: usize,
) {
    let narrow = content_width > 0 && content_width < 22;
    if identity.model.is_empty() {
        tokens.remove("quota_model");
        tokens.insert("quota_provider".to_string(), identity.provider.clone());
        tokens.insert(
            "quota_provider_model".to_string(),
            identity.provider.clone(),
        );
        return;
    }
    tokens.insert("quota_model".to_string(), identity.model.clone());
    if narrow {
        // Logo already says who; keep the model only and collapse a stacked
        // provider row so it does not re-introduce the prefix.
        tokens.remove("quota_provider");
        tokens.insert("quota_provider_model".to_string(), identity.model.clone());
    } else {
        tokens.insert("quota_provider".to_string(), identity.provider.clone());
        tokens.insert(
            "quota_provider_model".to_string(),
            format!("{}/{}", identity.provider, identity.model),
        );
    }
}

fn apply_context(
    tokens: &mut BTreeMap<String, String>,
    context: &ContextUsage,
    now_unix: u64,
    row: RowStyle,
) {
    insert_context_token(
        tokens,
        &crate::presentation::sidebar_context(Some(context), row.percent, row.shape),
        Some(crate::presentation::context_severity(context, row.percent)),
        row.shape,
    );
    let cache = crate::presentation::sidebar_cache(Some(context));
    if cache.is_empty() {
        tokens.remove("quota_cache");
    } else {
        tokens.insert("quota_cache".to_string(), cache);
    }
    for (name, value) in [
        (
            "quota_cache_ttl",
            crate::presentation::sidebar_cache_ttl(Some(context), now_unix),
        ),
        (
            "quota_cache_state",
            crate::presentation::sidebar_cache_state(Some(context), now_unix),
        ),
    ] {
        if value.is_empty() {
            tokens.remove(name);
        } else {
            tokens.insert(name.to_string(), value);
        }
    }
}

/// Keep the configured rows fixed. An empty TTL token collapses its row when
/// both visible fields fit inside the cache token; the next refresh can split
/// them again from the session evidence without rewriting Herdr's config.
///
/// `no cached` is not folded here: it keeps `$quota_cache_state` so the amber
/// warning colour survives. Gauges puts that token on the cache row.
fn fold_cache_row(tokens: &mut BTreeMap<String, String>, row: RowStyle) {
    use crate::cli::{SidebarField, SidebarLayout};
    if row.shape.layout != SidebarLayout::Gauges
        || !row.fields.contains(SidebarField::Cache)
        || !row.fields.contains(SidebarField::Ttl)
    {
        return;
    }
    let (Some(cache), Some(ttl)) = (tokens.get("quota_cache"), tokens.get("quota_cache_ttl"))
    else {
        return;
    };
    let joined = format!("{cache} · {ttl}");
    // These are plugin-generated numeric labels; · and ≈ each occupy one cell.
    if joined.chars().count() <= row.shape.content_width {
        tokens.insert("quota_cache".to_string(), joined);
        tokens.remove("quota_cache_ttl");
    }
}

/// True when the quota rows this pane is carrying are not the ones `values`
/// would render.
///
/// Only the window rows are comparable against a snapshot alone: publishing
/// rewrites provider, model, context and cache rows from pane-local evidence
/// this caller does not have, so including them would report drift that no
/// republish can settle.
///
/// A pane that has never been published to is not drift: waking those would
/// pull every quota-less pane into every pass. A pane that already carries
/// plugin tokens but no window row is drift once the snapshot has windows —
/// Claude/Agy statusLine only writes the mailbox, so an idle pane has no
/// other way to pick up a session that just reported quota.
pub(crate) fn quota_rows_have_drifted(
    current: &BTreeMap<String, String>,
    values: &MetadataTokens,
    shape: SidebarShape,
) -> bool {
    let desired = desired_tokens(values, "", shape);
    let current_has_window = QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| current.contains_key(name));
    let desired_has_window = QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| desired.contains_key(name));
    if !current_has_window {
        return desired_has_window && plugin_quota_present(current);
    }
    QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| current.get(name) != desired.get(name))
}

fn metadata_matches(
    current: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> bool {
    METADATA_TOKEN_NAMES
        .into_iter()
        .all(|name| current.get(name) == desired.get(name))
        && OBSOLETE_METADATA_TOKEN_NAMES
            .into_iter()
            .all(|name| !current.contains_key(name))
        && LEGACY_METADATA_TOKEN_NAMES
            .into_iter()
            .all(|name| !current.contains_key(name))
}

fn metadata_report_names(
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    rows: crate::cli::SidebarRows,
) -> Vec<&'static str> {
    let mut names = METADATA_TOKEN_NAMES
        .into_iter()
        .filter(|name| desired.contains_key(*name) || pane.tokens.contains_key(*name))
        .collect::<Vec<_>>();
    // Icon twins must always be named so an inactive colour is cleared even
    // when `agent list` omitted the stale token from `pane.tokens`.
    for name in ICON_TOKEN_NAMES {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let cleanup_names = OBSOLETE_METADATA_TOKEN_NAMES
        .into_iter()
        .filter(|name| pane.tokens.contains_key(*name))
        .chain(
            LEGACY_METADATA_TOKEN_NAMES
                .into_iter()
                .filter(|name| pane.tokens.contains_key(*name)),
        )
        .collect::<Vec<_>>();
    if names.len() + cleanup_names.len() <= MAX_METADATA_TOKENS {
        names.extend(cleanup_names);
        return names;
    }

    // Herdr accepts at most sixteen token arguments. Reserve room for stale
    // names first so an upgraded pane can actually clear them; an unchanged
    // value is re-sent on the next bounded report instead.
    let active_capacity = MAX_METADATA_TOKENS.saturating_sub(cleanup_names.len());
    while names.len() > active_capacity {
        let Some(index) = names
            .iter()
            .position(|name| !must_not_lag(pane, desired, rows, name))
        else {
            break;
        };
        names.remove(index);
    }
    // Still over: keep the protected names, whatever their list position.
    names.sort_by_key(|name| !must_not_lag(pane, desired, rows, name));
    names.truncate(active_capacity);
    names.extend(cleanup_names);
    names
}

/// Names a report over budget never defers. Dropping a name the pane still
/// carries but no longer wants would leave that row on screen forever, and a
/// stale icon colour is worse than a briefly lagged quota digit. Beyond
/// those, protect what the installed rows show: with identity rows the usage
/// values are hidden and may lag, but the header, the sort key the view and
/// its heads agree on, the Space-closing gap, the logo marks and the
/// identity fields may not.
fn must_not_lag(
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    rows: crate::cli::SidebarRows,
    name: &str,
) -> bool {
    let must_clear = pane.tokens.contains_key(name) && !desired.contains_key(name);
    // The views sort on `al_grp`, `al_role_rank` and `al_sort` under either
    // row set.
    must_clear
        || ICON_REPORT_NAMES.contains(&name)
        || name == SORT_TOKEN
        || name == GROUP_TOKEN
        || name == ROLE_RANK_TOKEN
        || match rows {
            crate::cli::SidebarRows::Quota => ROWS_THAT_MUST_NOT_LAG.contains(&name),
            crate::cli::SidebarRows::Identity => IDENTITY_ROWS_THAT_MUST_NOT_LAG.contains(&name),
        }
}

fn week_style_base(quota_5h: &str) -> &'static str {
    // Empty 5h publishes week beside context (`context · 7d`). A present 5h
    // keeps week on the limits row so 5h never shares a line with context.
    if quota_5h.trim().is_empty() {
        "quota_week_inline"
    } else {
        "quota_week"
    }
}

/// Publish the context row into the one name its layout and severity choose,
/// and clear the other three.
///
/// The clear is the point: a severity change or a layout switch moves the
/// value to a different name, and a pane that kept the old one would show two
/// context rows at once.
fn insert_context_token(
    tokens: &mut BTreeMap<String, String>,
    value: &str,
    severity: Option<crate::model::Severity>,
    shape: SidebarShape,
) {
    for name in CONTEXT_TOKEN_NAMES {
        tokens.remove(name);
    }
    if value.trim().is_empty() {
        return;
    }
    tokens.insert(
        context_token_name(shape, severity).to_string(),
        value.to_string(),
    );
}

fn context_token_name(
    shape: SidebarShape,
    severity: Option<crate::model::Severity>,
) -> &'static str {
    if shape.layout != crate::cli::SidebarLayout::Gauges {
        return "quota_context";
    }
    // `Severity::for_context_remaining` never returns `Unknown`, and a caller
    // with no severity has no coloured band to claim, so both read as normal.
    match severity {
        Some(crate::model::Severity::Warning) => "quota_context_warning",
        Some(crate::model::Severity::Danger) => "quota_context_danger",
        _ => "quota_context_normal",
    }
}

fn insert_severity_token(
    tokens: &mut BTreeMap<String, String>,
    base: &str,
    value: &str,
    severity: Option<crate::model::Severity>,
) {
    if value.trim().is_empty() {
        return;
    }
    let variant = severity_variant(severity);
    tokens.insert(format!("{base}_{variant}"), value.to_string());
}

fn severity_variant(severity: Option<crate::model::Severity>) -> &'static str {
    match severity.unwrap_or(crate::model::Severity::Unknown) {
        crate::model::Severity::Normal => "normal",
        crate::model::Severity::Warning => "warning",
        crate::model::Severity::Danger => "danger",
        crate::model::Severity::Unknown => "unknown",
    }
}

fn insert_optional_token(tokens: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if !value.trim().is_empty() {
        tokens.insert(name.to_string(), value.to_string());
    }
}

// `recent` rebuilds the pane's wrapped scrollback, which takes seconds and
// repaints the pane: the agent's terminal visibly scrolls, once per read.
// `visible` is the current screen only, costs microseconds, and repaints
// nothing. The prompt is on screen at the moment idle->working fires, which is
// exactly when the topic changes; later in the turn it may have scrolled off,
// and then the caller keeps the topic it already published.
fn topic_read_args(pane_id: &str) -> [&str; 7] {
    [
        "pane", "read", pane_id, "--source", "visible", "--format", "text",
    ]
}

/// Only harnesses whose prompt line [`prompt_candidate`] recognises are ever
/// read. Checked before the read, not after: `pane read` itself is the cost
/// (CLAUDE.md, "reading or writing a pane is not free"), and a Cursor pane's
/// evidence lives in its own session files, not on screen.
fn reads_screen_topic(harness: Harness) -> bool {
    matches!(harness, Harness::Claude | Harness::Codex)
}

fn read_pane_topic(executable: &std::ffi::OsStr, pane: &AgentPane) -> Option<String> {
    let output = Command::new(executable)
        .args(topic_read_args(&pane.pane_id))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    extract_topic(&text, pane.harness)
}

fn extract_topic(text: &str, harness: Harness) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let cleaned_line = strip_control_chars(line);
        let line = cleaned_line.trim();
        let candidate = prompt_candidate(line, harness)?;
        if candidate.is_empty() || is_status_line(candidate) {
            return None;
        }
        Some(truncate_topic(candidate))
    })
}

fn prompt_candidate(line: &str, harness: Harness) -> Option<&str> {
    let marker = match harness {
        Harness::Claude if line.starts_with('❯') => '❯',
        Harness::Codex if line.starts_with('›') => '›',
        _ => return None,
    };
    Some(line.trim_start_matches(marker).trim())
}

fn truncate_topic(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= 80 {
        return value.to_string();
    }
    let mut topic: String = characters.into_iter().take(77).collect();
    topic.push('…');
    topic
}

fn strip_control_chars(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .collect()
}

fn is_status_line(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("accept-edits mode:")
        || lower.starts_with("context ")
        || lower.starts_with("session ")
        || lower.starts_with("auto mode")
        || lower.starts_with("shift+tab")
        || lower == "ask codex to do anything"
        || matches!(
            lower.as_str(),
            "/clear" | "/compact" | "/help" | "/status" | "/usage" | "/model" | "/config"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attention_pane(pane_id: &str, headroom: &str, status: AgentStatus) -> AgentPane {
        AgentPane {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap().to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([
                (HEADROOM_TOKEN.to_string(), headroom.to_string()),
                ("quota_5h_normal".to_string(), "5h 60%".to_string()),
            ]),
            status,
            focused: false,
            state_labels: BTreeMap::new(),
        }
    }

    /// Five seats in two Spaces with the owner's example group; `w1` is
    /// Herdr's workspace 2 and `w2` its workspace 1, so the workspace number,
    /// not the id, decides a custom group's head.
    fn grouped_board(
        order: crate::cli::AgentOrder,
    ) -> (Vec<AgentPane>, BTreeMap<String, String>, Grouping) {
        let seat = |id: &str, headroom: &str, label: &str| {
            let mut pane = attention_pane(id, headroom, AgentStatus::Idle);
            pane.tokens.insert(
                SORT_TOKEN.to_string(),
                sort_key(headroom.parse().unwrap(), id),
            );
            (pane, (id.to_string(), label.to_string()))
        };
        let (panes, labels): (Vec<_>, BTreeMap<_, _>) = [
            seat("w1:p1", "050", "hub.alioth.统筹.0925-2307.opus-5-5@cpu-box"),
            seat(
                "w1:p2",
                "010",
                "spk.alioth.推理验收.0925-2307.opus-5-5@cpu-box",
            ),
            seat("w2:p1", "080", "hub.dubhe.main.0925-2308.fable-5-1@cpu-box"),
            seat(
                "w2:p2",
                "020",
                "spk.dubhe.归档打分.0925-2305.ds-v41-flash@cpu-box",
            ),
            seat(
                "w2:p3",
                "030",
                "spk.dubhe.天枢接力.0925-2304.ds-v41-flash@cpu-box",
            ),
        ]
        .into_iter()
        .unzip();
        let grouping = Grouping::new(
            order,
            crate::groups::Groups::parse("中枢 = role:统筹, role:秘书, role:hub"),
            &panes,
            Some(&labels),
            BTreeMap::from([("w1".to_string(), 2), ("w2".to_string(), 1)]),
        );
        (panes, labels, grouping)
    }

    #[test]
    fn a_custom_group_sits_above_the_spaces_with_its_own_head() {
        use crate::cli::AgentOrder;
        let (panes, _, grouping) = grouped_board(AgentOrder::Quota);
        let heads = space_group_heads(AgentOrder::Quota, &grouping, &panes, &panes, &[]);
        // 中枢 spans both Spaces: workspace 1 (w2) comes first, so the dubhe
        // hub heads it even with more quota left than the coordinator.
        assert_eq!(heads.get("#C00").map(String::as_str), Some("w2:p1"));
        // What is left of each Space heads itself, tightest first.
        assert_eq!(heads.get("w1").map(String::as_str), Some("w1:p2"));
        assert_eq!(heads.get("w2").map(String::as_str), Some("w2:p2"));
        assert_eq!(heads.len(), 3);
        let labels = BTreeMap::from([
            ("w1".to_string(), "alioth".to_string()),
            ("w2".to_string(), "dubhe".to_string()),
        ]);
        let hub = &panes[2];
        let coordinator = &panes[0];
        assert_eq!(
            group_label_for(hub, &grouping, &heads, &labels).as_deref(),
            Some("中枢")
        );
        assert_eq!(
            group_label_for(coordinator, &grouping, &heads, &labels),
            None
        );
    }

    #[test]
    fn every_seat_carries_its_group_key_and_a_member_names_its_space() {
        use crate::cli::AgentOrder;
        let (panes, _, grouping) = grouped_board(AgentOrder::Quota);
        let labels = BTreeMap::from([("w1".to_string(), "alioth".to_string())]);
        let mut member = BTreeMap::from([(ROLE_TOKEN.to_string(), "统筹".to_string())]);
        apply_grouping(&mut member, &panes[0], &grouping, &labels);
        assert_eq!(member.get(GROUP_TOKEN).map(String::as_str), Some("C00"));
        assert_eq!(member.get(ROLE_RANK_TOKEN).map(String::as_str), Some("0"));
        assert_eq!(member.get(SPACE_TOKEN).map(String::as_str), Some("alioth"));
        let mut spoke = BTreeMap::from([(ROLE_TOKEN.to_string(), "spoke".to_string())]);
        apply_grouping(&mut spoke, &panes[1], &grouping, &labels);
        assert_eq!(spoke.get(GROUP_TOKEN).map(String::as_str), Some("Z"));
        assert_eq!(spoke.get(ROLE_RANK_TOKEN).map(String::as_str), Some("4"));
        assert!(!spoke.contains_key(SPACE_TOKEN));

        // The value does not follow the order, so toggling writes nothing;
        // under attention nobody is grouped.
        let (panes, _, flat) = grouped_board(AgentOrder::Attention);
        let mut desired = BTreeMap::new();
        apply_grouping(&mut desired, &panes[0], &flat, &labels);
        assert_eq!(desired.get(GROUP_TOKEN).map(String::as_str), Some("C00"));
        assert_eq!(flat.gid(&panes[0]), "w1");
    }

    #[test]
    fn an_unlabelled_pane_keeps_the_group_it_carries() {
        use crate::cli::AgentOrder;
        let (mut panes, _, _) = grouped_board(AgentOrder::Quota);
        panes[1]
            .tokens
            .insert(GROUP_TOKEN.to_string(), "C00".to_string());
        // Label read failed: nothing is regrouped, and a pane the view lists
        // in 中枢 stays there.
        let grouping = Grouping::new(
            AgentOrder::Quota,
            crate::groups::Groups::parse("中枢 = role:hub"),
            &panes,
            None,
            BTreeMap::new(),
        );
        assert_eq!(grouping.group_value(&panes[1]), None);
        assert_eq!(grouping.gid(&panes[1]), "#C00");
        let mut desired = BTreeMap::new();
        apply_grouping(&mut desired, &panes[1], &grouping, &BTreeMap::new());
        assert_eq!(desired.get(GROUP_TOKEN).map(String::as_str), Some("C00"));
    }

    #[test]
    fn the_role_order_heads_a_space_with_its_highest_role() {
        use crate::cli::AgentOrder;
        let (panes, _, _) = grouped_board(AgentOrder::Role);
        let grouping = Grouping::new(
            AgentOrder::Role,
            crate::groups::Groups::default(),
            &panes,
            Some(&grouped_board(AgentOrder::Role).1),
            BTreeMap::new(),
        );
        let heads = space_group_heads(AgentOrder::Role, &grouping, &panes, &panes, &[]);
        // 统筹 before the tighter spoke; the dubhe hub before its spokes.
        assert_eq!(heads.get("w1").map(String::as_str), Some("w1:p1"));
        assert_eq!(heads.get("w2").map(String::as_str), Some("w2:p1"));
        let quota = Grouping::new(
            AgentOrder::Quota,
            crate::groups::Groups::default(),
            &panes,
            Some(&grouped_board(AgentOrder::Quota).1),
            BTreeMap::new(),
        );
        let heads = space_group_heads(AgentOrder::Quota, &quota, &panes, &panes, &[]);
        assert_eq!(heads.get("w1").map(String::as_str), Some("w1:p2"));
    }

    #[test]
    fn the_attention_rank_follows_the_icon_colour_and_rides_on_icon_reports() {
        let look = Look::plain();
        let grouping = Grouping::spaces(crate::cli::AgentOrder::Attention);
        for (status, rank, icon) in [
            (AgentStatus::Done, "1", "quota_icon_done"),
            (AgentStatus::Working, "2", "quota_icon_working"),
            (AgentStatus::Idle, "3", "quota_icon"),
            (AgentStatus::Blocked, "0", "quota_icon"),
        ] {
            let pane = attention_pane("w1:p1", "050", status);
            let mut desired = BTreeMap::new();
            apply_group_and_icon(
                &mut desired,
                &pane,
                &grouping,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &look,
            );
            assert_eq!(desired.get(ATTENTION_TOKEN).map(String::as_str), Some(rank));
            assert!(desired.contains_key(icon), "{status:?}");
            // A colour change is an icon report, and so is a rank change.
            let mut seen = pane.clone();
            seen.tokens = desired.clone();
            assert!(icon_tokens_match(&seen.tokens, &desired));
            assert!(!seen.icon_needs_update(), "{status:?}");
            seen.tokens.remove(ATTENTION_TOKEN);
            assert!(seen.icon_needs_update(), "{status:?} without a rank");
        }
    }

    #[test]
    fn a_seat_row_shows_the_first_segment_of_its_space_unless_it_collides() {
        let labels = BTreeMap::from([
            ("w1".to_string(), "siflow-inference-lab".to_string()),
            ("w2".to_string(), "m1-release-sprint".to_string()),
            ("w3".to_string(), "m1-eval".to_string()),
            ("w4".to_string(), "alioth".to_string()),
            ("w5".to_string(), "玉衡".to_string()),
            ("w6".to_string(), "-odd".to_string()),
        ]);
        let short = short_space_names(&labels);
        assert_eq!(short["w1"], "siflow");
        assert_eq!(short["w2"], "m1-release-sprint", "m1 is ambiguous");
        assert_eq!(short["w3"], "m1-eval");
        assert_eq!(short["w4"], "alioth");
        assert_eq!(short["w5"], "玉衡");
        assert_eq!(short["w6"], "-odd");
    }

    fn our_labels() -> BTreeMap<String, String> {
        ATTENTION_STATE_LABELS
            .into_iter()
            .map(|(status, text)| (status.to_string(), text.to_string()))
            .collect()
    }

    #[test]
    fn the_attention_view_is_flat_and_ranks_needs_you_first() {
        use crate::cli::AgentOrder;
        let request = agent_view_request(AgentOrder::Attention);
        assert_eq!(request["method"], "agent.view.set");
        assert_eq!(request["params"]["source"], "plugin:herdr-agent-quota");
        assert_eq!(request["params"]["label"], "attention");
        // The plugin's own colour rank, not Herdr's `attention`: they
        // disagree on a same-tab sibling that is still teal (owner 0928:
        // green first, then 中枢 before the rest).
        assert_eq!(
            request["params"]["sort"],
            serde_json::json!([
                {"field": {"token": "al_att"}, "order": "asc"},
                {"field": {"token": "al_grp"}, "order": "asc"},
                {"field": "workspace_order", "order": "asc"},
                {"field": {"token": "al_sort"}, "order": "asc"},
            ])
        );
    }

    fn looked(
        group: crate::look::GroupStyle,
        logos: crate::look::LogoLayout,
        inventory: &[AgentPane],
    ) -> Look {
        Look {
            group,
            logos,
            counts: space_counts(&Grouping::spaces(crate::cli::AgentOrder::Quota), inventory),
            ..Look::plain()
        }
    }

    #[test]
    fn a_counted_header_and_its_gap_land_on_the_head_only() {
        use crate::look::{GroupStyle, LogoLayout};
        let head = attention_pane("w1:p1", "004", AgentStatus::Blocked);
        let done = attention_pane("w1:p2", "050", AgentStatus::Done);
        let member = attention_pane("w1:p3", "090", AgentStatus::Working);
        let inventory = vec![head.clone(), done.clone(), member.clone()];
        let heads = group_head_pane_ids(
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &inventory,
            &inventory,
            &[],
        );
        let spaces = BTreeMap::from([("w1".to_string(), "alioth".to_string())]);
        let mut look = looked(GroupStyle::Count, LogoLayout::None, &inventory);
        look.tails = space_ends(
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &inventory,
            &inventory,
            &[],
        )
        .into_values()
        .map(|(_, tail)| tail)
        .collect();
        assert_eq!(look.tails, BTreeSet::from(["w1:p3".to_string()]));
        let mut desired = BTreeMap::new();
        apply_group_and_icon(
            &mut desired,
            &head,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &spaces,
            &look,
        );
        assert_eq!(
            desired.get("quota_group").map(String::as_str),
            Some("alioth · 3 席 · 1 卡住 · 1 已回")
        );
        assert!(!desired.contains_key(GAP_TOKEN), "the head carries no gap");
        let mut tail = BTreeMap::from([("quota_group".to_string(), "stale".to_string())]);
        apply_group_and_icon(
            &mut tail,
            &member,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &spaces,
            &look,
        );
        assert!(!tail.contains_key("quota_group"));
        assert_eq!(
            tail.get(GAP_TOKEN).map(String::as_str),
            Some(crate::look::GAP)
        );

        let bar = looked(GroupStyle::Bar, LogoLayout::None, &inventory);
        let mut desired = BTreeMap::new();
        apply_group_and_icon(
            &mut desired,
            &head,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &spaces,
            &bar,
        );
        assert_eq!(
            desired.get("quota_group").map(String::as_str),
            Some("▌ alioth 3席")
        );
        assert!(!desired.contains_key(GAP_TOKEN));

        let plain = Look::plain();
        let mut desired = BTreeMap::new();
        apply_group_and_icon(
            &mut desired,
            &head,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &spaces,
            &plain,
        );
        assert_eq!(
            desired.get("quota_group").map(String::as_str),
            Some("alioth")
        );
        assert!(!desired.contains_key(GAP_TOKEN));
    }

    fn process(route: Option<&str>, official_home: bool) -> crate::channel::ProcessChannel {
        crate::channel::ProcessChannel {
            route: route.map(str::to_string),
            official_home,
            registered_codex_home: false,
        }
    }

    #[test]
    fn logos_show_harness_channel_and_model_and_keep_the_star_for_a_ds_base() {
        use crate::look::{GroupStyle, LogoLayout};
        let mut pane = attention_pane("w2:p1", "055", AgentStatus::Working);
        let sessionless = pane.clone();
        pane.session = Some(AgentSession {
            kind: None,
            value: "s-demo".to_string(),
        });
        let look = looked(
            GroupStyle::Plain,
            LogoLayout::Inline,
            std::slice::from_ref(&pane),
        );
        let channels =
            BTreeMap::from([("w2:p1".to_string(), process(Some("127.0.0.1:8686"), false))]);
        // The label says op5; the session reported DeepSeek. The model mark
        // follows the session.
        let mut desired = BTreeMap::from([
            (MODEL_TOKEN.to_string(), "op5".to_string()),
            ("quota_model".to_string(), "deepseek-v4.1-flash".to_string()),
        ]);
        apply_logos(&mut desired, &pane, &look, Some(&channels));
        apply_group_and_icon(
            &mut desired,
            &pane,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &look,
        );
        assert_eq!(
            desired.get(CHANNEL_TOKEN).map(String::as_str),
            Some("\u{e1bf}")
        );
        assert_eq!(
            desired.get(MODEL_MARK_TOKEN).map(String::as_str),
            Some("\u{e1bc}")
        );
        assert_eq!(
            desired.get("quota_icon_working").map(String::as_str),
            Some("\u{e1b7}"),
            "with logos on the first mark is the harness, not the whale"
        );

        // An official lane's process: the channel is Anthropic's logo, not
        // text; GLM is the z.ai glyph.
        let official = BTreeMap::from([("w2:p1".to_string(), process(None, true))]);
        let mut desired = BTreeMap::from([("quota_model".to_string(), "GLM-5.3".to_string())]);
        apply_logos(&mut desired, &pane, &look, Some(&official));
        assert_eq!(
            desired.get(CHANNEL_TOKEN).map(String::as_str),
            Some("\u{e1c6}")
        );
        assert_eq!(
            desired.get(MODEL_MARK_TOKEN).map(String::as_str),
            Some("\u{e1bb}")
        );

        // No process, no session model: a label naming a model (or the
        // Codex coordinator's `codex-home`) proves neither. Old marks clear.
        for label_model in ["ds-v41-flash", "codex-home"] {
            let mut desired = BTreeMap::from([
                (MODEL_TOKEN.to_string(), label_model.to_string()),
                (CHANNEL_TOKEN.to_string(), "\u{e1bf}".to_string()),
                (MODEL_MARK_TOKEN.to_string(), "\u{e1bc}".to_string()),
            ]);
            apply_logos(&mut desired, &pane, &look, Some(&BTreeMap::new()));
            assert!(!desired.contains_key(CHANNEL_TOKEN), "{label_model}");
            assert!(!desired.contains_key(MODEL_MARK_TOKEN), "{label_model}");
        }
        // A session-less pane's preserved `quota_model` is not evidence.
        let mut desired = BTreeMap::from([("quota_model".to_string(), "Fable 5.1".to_string())]);
        apply_logos(&mut desired, &sessionless, &look, Some(&channels));
        assert!(!desired.contains_key(MODEL_MARK_TOKEN));
        // An unset base URL outside an official lane is not official.
        let bare = BTreeMap::from([("w2:p1".to_string(), process(None, false))]);
        let mut desired = BTreeMap::new();
        apply_logos(&mut desired, &pane, &look, Some(&bare));
        assert!(!desired.contains_key(CHANNEL_TOKEN));

        // Logos off: both marks are cleared and the whale is back.
        let off = Look::plain();
        let mut desired = BTreeMap::from([
            (MODEL_TOKEN.to_string(), "ds-v41-flash".to_string()),
            (CHANNEL_TOKEN.to_string(), "x".to_string()),
            (MODEL_MARK_TOKEN.to_string(), "y".to_string()),
        ]);
        apply_logos(&mut desired, &pane, &off, Some(&channels));
        apply_group_and_icon(
            &mut desired,
            &pane,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &off,
        );
        assert!(!desired.contains_key(CHANNEL_TOKEN));
        assert!(!desired.contains_key(MODEL_MARK_TOKEN));
        assert_eq!(
            desired.get("quota_icon_working").map(String::as_str),
            Some("\u{e1bc}")
        );
    }

    /// What one pass's refresh carries for a pane (defect-0 scenarios).
    #[derive(Clone, Copy)]
    enum Update {
        Headroom(Option<u8>),
        Clear,
        Preserve,
    }

    /// Herdr's side of the publish pass for the defect-0 scenarios: the
    /// tokens each pane holds and which panes are scrolled. Herdr I/O is
    /// replaced by writes to this board; the decisions go through the same
    /// helpers `publish_pane_tokens_inner` and `sync_sibling_group_headers`
    /// use, in the same order (heads, scroll hold, re-chosen heads, writes,
    /// sibling headers).
    struct Board {
        panes: Vec<AgentPane>,
        scrolled: BTreeSet<String>,
        labels: BTreeMap<String, String>,
        look: Look,
    }

    impl Board {
        fn new(seats: &[(&str, Option<u8>, AgentStatus)]) -> Self {
            let panes = seats
                .iter()
                .map(|(id, headroom, status)| {
                    let mut pane = attention_pane(id, "", *status);
                    pane.tokens.remove(HEADROOM_TOKEN);
                    if let Some(headroom) = headroom {
                        pane.tokens
                            .insert(HEADROOM_TOKEN.to_string(), format!("{headroom:03}"));
                    }
                    pane
                })
                .collect::<Vec<_>>();
            let mut board = Self {
                panes,
                scrolled: BTreeSet::new(),
                labels: BTreeMap::from([
                    ("w1".to_string(), "alioth".to_string()),
                    ("w2".to_string(), "lab".to_string()),
                ]),
                look: Look::plain(),
            };
            let all = board
                .panes
                .iter()
                .map(|pane| (pane.pane_id.clone(), Update::Preserve))
                .collect::<Vec<_>>();
            board.pass(crate::cli::AgentOrder::Quota, &all);
            board
        }

        fn pane(&mut self, id: &str) -> &mut AgentPane {
            self.panes
                .iter_mut()
                .find(|pane| pane.pane_id == id)
                .unwrap()
        }

        fn pass(&mut self, order: crate::cli::AgentOrder, updates: &[(String, Update)]) {
            let inventory = self.panes.clone();
            let publishing = inventory
                .iter()
                .filter(|pane| updates.iter().any(|(id, _)| id == &pane.pane_id))
                .cloned()
                .collect::<Vec<_>>();
            let tokens = updates
                .iter()
                .map(|(id, update)| PaneTokens {
                    pane_id: id.clone(),
                    quota: match update {
                        Update::Headroom(headroom) => {
                            let mut values = crate::presentation::MetadataTokens::unavailable(
                                crate::model::Provider::Claude,
                                "demo",
                            );
                            values.quota_headroom = *headroom;
                            PaneQuotaUpdate::Replace(Box::new(values))
                        }
                        Update::Clear => PaneQuotaUpdate::Clear,
                        Update::Preserve => PaneQuotaUpdate::Preserve,
                    },
                    identity: None,
                    context: None,
                })
                .collect::<Vec<_>>();
            let labels = self.labels.clone();
            let mut look = self.look_copy();
            look.counts =
                space_counts(&Grouping::spaces(crate::cli::AgentOrder::Quota), &inventory);
            look.tails = space_tails(
                order,
                look.group,
                &Grouping::spaces(order),
                &inventory,
                &publishing,
                &tokens,
            );
            let mut heads = space_group_heads(
                order,
                &Grouping::spaces(order),
                &inventory,
                &publishing,
                &tokens,
            );
            let desired_for = |pane: &AgentPane, heads: &BTreeMap<String, String>, look: &Look| {
                let mut desired = pane.tokens.clone();
                match updates
                    .iter()
                    .find(|(id, _)| id == &pane.pane_id)
                    .unwrap()
                    .1
                {
                    Update::Headroom(Some(headroom)) => {
                        desired.insert(HEADROOM_TOKEN.to_string(), format!("{headroom:03}"));
                    }
                    Update::Headroom(None) | Update::Clear => {
                        desired.remove(HEADROOM_TOKEN);
                    }
                    Update::Preserve => {}
                }
                apply_group_and_icon(
                    &mut desired,
                    pane,
                    &Grouping::spaces(crate::cli::AgentOrder::Quota),
                    heads,
                    &labels,
                    look,
                );
                let headroom = desired
                    .get(HEADROOM_TOKEN)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(u8::MAX);
                desired.insert(SORT_TOKEN.to_string(), sort_key(headroom, &pane.pane_id));
                desired
            };
            let held = publishing
                .iter()
                .filter(|pane| self.scrolled.contains(&pane.pane_id))
                .filter(|pane| !metadata_matches(&pane.tokens, &desired_for(pane, &heads, &look)))
                .map(|pane| pane.pane_id.clone())
                .collect::<BTreeSet<_>>();
            if !held.is_empty() {
                let settled = publishing
                    .iter()
                    .filter(|pane| !held.contains(&pane.pane_id))
                    .cloned()
                    .collect::<Vec<_>>();
                heads = space_group_heads(
                    order,
                    &Grouping::spaces(order),
                    &inventory,
                    &settled,
                    &tokens,
                );
                look.tails = space_tails(
                    order,
                    look.group,
                    &Grouping::spaces(order),
                    &inventory,
                    &settled,
                    &tokens,
                );
            }
            for pane in &publishing {
                let desired = desired_for(pane, &heads, &look);
                if self.scrolled.contains(&pane.pane_id) {
                    // Icon-only write while scrolled.
                    let target = self.pane(&pane.pane_id);
                    for name in ICON_TOKEN_NAMES {
                        match desired.get(name) {
                            Some(value) => target.tokens.insert(name.to_string(), value.clone()),
                            None => target.tokens.remove(name),
                        };
                    }
                    continue;
                }
                self.pane(&pane.pane_id).tokens = desired;
            }
            let touched = publishing
                .iter()
                .map(|pane| pane.workspace_id.clone())
                .collect::<BTreeSet<_>>();
            for sibling in &inventory {
                if updates.iter().any(|(id, _)| id == &sibling.pane_id)
                    || !touched.contains(&sibling.workspace_id)
                    || self.scrolled.contains(&sibling.pane_id)
                {
                    continue;
                }
                let mut wanted = BTreeMap::new();
                let header = group_label_for(
                    sibling,
                    &Grouping::spaces(crate::cli::AgentOrder::Quota),
                    &heads,
                    &labels,
                )
                .map(|name| look.header(&sibling.workspace_id, &name));
                apply_header(&mut wanted, &sibling.pane_id, header, &look);
                let target = self.pane(&sibling.pane_id);
                for name in ["quota_group", GAP_TOKEN] {
                    match wanted.get(name) {
                        Some(value) => target.tokens.insert(name.to_string(), value.clone()),
                        None => target.tokens.remove(name),
                    };
                }
            }
        }

        fn look_copy(&self) -> Look {
            Look {
                style: self.look.style,
                group: self.look.group,
                logos: self.look.logos,
                rows: self.look.rows,
                counts: BTreeMap::new(),
                tails: BTreeSet::new(),
            }
        }

        fn refresh(&mut self, updates: &[(&str, Update)]) {
            let updates = updates
                .iter()
                .map(|(id, update)| (id.to_string(), *update))
                .collect::<Vec<_>>();
            self.pass(crate::cli::AgentOrder::Quota, &updates);
        }

        fn refresh_all(&mut self, order: crate::cli::AgentOrder) {
            let all = self
                .panes
                .iter()
                .map(|pane| (pane.pane_id.clone(), Update::Preserve))
                .collect::<Vec<_>>();
            self.pass(order, &all);
        }

        /// The grouped view as Herdr 0.9.1 lists it: `workspace_order`, then
        /// `al_sort` ascending, a missing token last.
        fn view(&self) -> Vec<&AgentPane> {
            let mut rows = self.panes.iter().collect::<Vec<_>>();
            rows.sort_by_key(|pane| {
                let key = pane.tokens.get(SORT_TOKEN);
                (pane.workspace_id.clone(), key.is_none(), key.cloned())
            });
            rows
        }

        /// Every Space: exactly one head, and it is the first pane listed.
        fn assert_heads(&self, when: &str) {
            let view = self.view();
            let spaces = view
                .iter()
                .map(|pane| pane.workspace_id.clone())
                .collect::<BTreeSet<_>>();
            for space in spaces {
                let members = view
                    .iter()
                    .filter(|pane| pane.workspace_id == space)
                    .collect::<Vec<_>>();
                // A pane held by scrollback cannot be written, so a header
                // it already carried stays until the user scrolls back.
                let heads = members
                    .iter()
                    .filter(|pane| pane.tokens.contains_key("quota_group"))
                    .filter(|pane| !self.scrolled.contains(&pane.pane_id))
                    .map(|pane| pane.pane_id.as_str())
                    .collect::<Vec<_>>();
                let order = members
                    .iter()
                    .map(|pane| pane.pane_id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    heads.len(),
                    1,
                    "{when}: {space} heads {heads:?} in {order:?}"
                );
                assert_eq!(
                    heads[0], order[0],
                    "{when}: bare pane above the head in {order:?}"
                );
            }
        }

        fn head(&self, space: &str) -> String {
            self.view()
                .into_iter()
                .find(|pane| pane.workspace_id == space && pane.tokens.contains_key("quota_group"))
                .map(|pane| pane.pane_id.clone())
                .unwrap_or_default()
        }
    }

    #[test]
    fn defect_zero_one_head_per_space_and_no_bare_pane_above_it() {
        use AgentStatus::{Done, Idle, Working};
        let mut board = Board::new(&[
            ("w1:p3P", Some(4), Idle),
            ("w1:p3N", Some(4), Done),
            ("w1:p55", Some(81), Working),
            ("w2:p7", Some(40), Idle),
            ("w2:p8", None, Idle),
        ]);
        board.assert_heads("start: tie at 004");
        assert_eq!(
            board.head("w1"),
            "w1:p3N",
            "the tie goes to the smaller key"
        );

        // Head without quota: its refresh carries no headroom.
        board.refresh(&[("w1:p3N", Update::Headroom(None))]);
        board.assert_heads("head lost its quota");
        assert_eq!(board.head("w1"), "w1:p3P");
        board.refresh(&[("w1:p3P", Update::Clear)]);
        board.assert_heads("head quota cleared");
        assert_eq!(board.head("w1"), "w1:p55");

        // A Space where no pane has quota still gets one head.
        board.refresh(&[("w2:p7", Update::Headroom(None))]);
        board.assert_heads("no quota in the Space");

        // A status change publishes one pane only.
        board.pane("w1:p55").status = AgentStatus::Blocked;
        board.refresh(&[("w1:p55", Update::Preserve)]);
        board.assert_heads("status change");

        // A single-pane refresh that moves the head: siblings follow.
        board.refresh(&[("w1:p3P", Update::Headroom(Some(2)))]);
        board.assert_heads("single-pane refresh takes the head");
        assert_eq!(board.head("w1"), "w1:p3P");

        // Scrollback blocks the new head's write: the head stays where the
        // view still lists first, instead of vanishing.
        board.scrolled.insert("w1:p55".to_string());
        board.refresh(&[
            ("w1:p55", Update::Headroom(Some(1))),
            ("w1:p3P", Update::Headroom(Some(2))),
        ]);
        board.assert_heads("scrollback held the would-be head");
        assert_eq!(board.head("w1"), "w1:p3P");
        // Scrolled back: the next refresh moves it.
        board.scrolled.clear();
        board.refresh(&[("w1:p55", Update::Headroom(Some(1)))]);
        board.assert_heads("after scrolling back");
        assert_eq!(board.head("w1"), "w1:p55");

        // Scrollback on the old head while another pane takes over: the
        // held pane keeps its old header until it can be written.
        board.scrolled.insert("w1:p55".to_string());
        board.refresh(&[
            ("w1:p55", Update::Headroom(Some(90))),
            ("w1:p3N", Update::Headroom(Some(0))),
        ]);
        board.assert_heads("old head scrolled");
        assert_eq!(board.head("w1"), "w1:p3N", "the new head is first at once");
        board.scrolled.clear();
        board.refresh(&[("w1:p55", Update::Headroom(Some(90)))]);
        board.assert_heads("old head back");
        assert_eq!(board.head("w1"), "w1:p3N");

        // Attention round trip: every header goes, and the way back puts
        // exactly one on the current first pane; the old head is cleared.
        board.refresh_all(crate::cli::AgentOrder::Attention);
        assert!(board
            .panes
            .iter()
            .all(|pane| !pane.tokens.contains_key("quota_group")));
        board.refresh(&[("w1:p3P", Update::Headroom(Some(0)))]);
        board.pass(
            crate::cli::AgentOrder::Attention,
            &[("w1:p3P".to_string(), Update::Headroom(Some(0)))],
        );
        board.refresh_all(crate::cli::AgentOrder::Quota);
        board.assert_heads("attention round trip");
        assert_eq!(board.head("w1"), "w1:p3N", "tie 000: smaller key");
        assert_eq!(
            board
                .panes
                .iter()
                .filter(|pane| pane.tokens.contains_key("quota_group"))
                .count(),
            2,
            "one head per Space, nothing left from before"
        );
    }

    #[test]
    fn a_pass_with_nothing_new_writes_nothing() {
        use AgentStatus::{Done, Idle};
        let mut board = Board::new(&[
            ("w1:p1", Some(10), Idle),
            ("w1:p2", Some(20), Done),
            ("w2:p3", None, Idle),
        ]);
        board.look.group = crate::look::GroupStyle::Count;
        board.look.logos = crate::look::LogoLayout::SecondRow;
        board.refresh_all(crate::cli::AgentOrder::Quota);
        let settled = board.panes.clone();
        // A second identical pass: every pane already matches, so the real
        // pass reports nothing and no revision moves.
        board.refresh_all(crate::cli::AgentOrder::Quota);
        for (before, after) in settled.iter().zip(&board.panes) {
            assert!(
                metadata_matches(&before.tokens, &after.tokens),
                "{} would be rewritten",
                before.pane_id
            );
        }
        // Turning logos and the counted header off clears their tokens.
        board.look = Look::plain();
        board.refresh_all(crate::cli::AgentOrder::Quota);
        for pane in &board.panes {
            for name in [GAP_TOKEN, CHANNEL_TOKEN, MODEL_MARK_TOKEN] {
                assert!(
                    !pane.tokens.contains_key(name),
                    "{} kept {name}",
                    pane.pane_id
                );
            }
        }
        board.assert_heads("plain again");
    }

    #[test]
    fn a_headroom_tie_puts_the_header_on_the_pane_the_view_lists_first() {
        // 0927: 统筹 (w1:p3P) and 秘书 (w1:p3N) both at 004. The view now sorts
        // on the same key the head is chosen by, so whichever pane wins the
        // tie is both first in the Space and the one carrying its header.
        let coord = attention_pane("w1:p3P", "004", AgentStatus::Idle);
        let sec = attention_pane("w1:p3N", "004", AgentStatus::Done);
        let loose = attention_pane("w1:p55", "081", AgentStatus::Working);
        let unknown = attention_pane("w1:p3T", "", AgentStatus::Idle);
        let mut unknown = unknown;
        unknown.tokens.remove(HEADROOM_TOKEN);
        let inventory = vec![coord.clone(), sec.clone(), loose.clone(), unknown.clone()];
        let heads = group_head_pane_ids(
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &inventory,
            &inventory,
            &[],
        );
        let head = heads.get("w1").unwrap();
        let mut keys = inventory
            .iter()
            .map(|pane| {
                let headroom = pane
                    .tokens
                    .get(HEADROOM_TOKEN)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(u8::MAX);
                (sort_key(headroom, &pane.pane_id), pane.pane_id.clone())
            })
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            &keys[0].1, head,
            "head must be the first pane the view lists"
        );
        assert_eq!(
            keys.last().unwrap().1,
            "w1:p3T",
            "unknown headroom sorts last"
        );
        assert_eq!(sort_key(4, "w1:p3N"), "004w1:p3N");
        assert_eq!(sort_key(u8::MAX, "w1:p3T"), "999w1:p3T");
        assert!(sort_key(100, "w9:p9") < sort_key(u8::MAX, "w0:p0"));
    }

    #[test]
    fn the_quota_view_is_unchanged_by_the_attention_order() {
        use crate::cli::AgentOrder;
        let request = agent_view_request(AgentOrder::Quota);
        assert_eq!(request["params"]["label"], "Quota by group");
        assert_eq!(
            request["params"]["sort"],
            serde_json::json!([
                {"field": {"token": "al_grp"}, "order": "asc"},
                {"field": "workspace_order", "order": "asc"},
                {"field": {"token": "al_sort"}, "order": "asc"},
            ])
        );
    }

    #[test]
    fn the_role_view_ranks_roles_inside_a_space_and_tightest_is_flat() {
        use crate::cli::AgentOrder;
        let role = agent_view_request(AgentOrder::Role);
        assert_eq!(role["params"]["label"], "Role by group");
        assert_eq!(
            role["params"]["sort"],
            serde_json::json!([
                {"field": {"token": "al_grp"}, "order": "asc"},
                {"field": "workspace_order", "order": "asc"},
                {"field": {"token": "al_role_rank"}, "order": "asc"},
                {"field": {"token": "al_sort"}, "order": "asc"},
            ])
        );
        let flat = agent_view_request(AgentOrder::Tightest);
        assert_eq!(
            flat["params"]["sort"],
            serde_json::json!([{"field": {"token": "al_sort"}, "order": "asc"}])
        );
    }

    #[test]
    fn state_labels_are_current_only_as_the_exact_full_set() {
        assert!(state_labels_current(&our_labels()));
        assert!(!state_labels_current(&BTreeMap::new()));
        let mut missing = our_labels();
        missing.remove("unknown");
        assert!(!state_labels_current(&missing));
        let mut edited = our_labels();
        edited.insert("done".to_string(), "done".to_string());
        assert!(!state_labels_current(&edited));
        let mut extra = our_labels();
        extra.insert("later".to_string(), "x".to_string());
        assert!(!state_labels_current(&extra));
    }

    #[test]
    fn labels_are_written_once_whatever_the_order() {
        let mut pane = attention_pane("w1:p1", "050", AgentStatus::Idle);
        assert_eq!(label_change(&pane), LabelChange::Write);
        pane.state_labels = our_labels();
        // Read back unchanged: no write, which is what keeps a refresh silent.
        assert_eq!(label_change(&pane), LabelChange::Keep);
        pane.state_labels = BTreeMap::from([("done".to_string(), "fin".to_string())]);
        assert_eq!(label_change(&pane), LabelChange::Write);
    }

    #[test]
    fn a_label_write_names_all_five_statuses_because_herdr_replaces_the_set() {
        let args = state_label_args(LabelChange::Write);
        let pairs = args
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair[0], "--state-label");
                pair[1].clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            [
                "blocked=[卡住]",
                "done=[已回]",
                "working=[工作]",
                "idle=[空闲]",
                "unknown=[未报]",
            ]
        );
        assert!(state_label_args(LabelChange::Keep).is_empty());
    }

    #[test]
    fn no_pending_decision_count_is_invented() {
        // `[待拍N]` has no per-pane source; nothing may print it.
        assert!(ATTENTION_STATE_LABELS
            .into_iter()
            .all(|(_, text)| !text.contains('拍')));
    }

    #[test]
    fn the_attention_order_publishes_no_space_header_and_no_indent() {
        use crate::cli::AgentOrder;
        let tight = attention_pane("w1:p1", "010", AgentStatus::Done);
        let loose = attention_pane("w1:p2", "090", AgentStatus::Working);
        let inventory = vec![tight.clone(), loose.clone()];
        let labels = BTreeMap::from([("w1".to_string(), "玉衡".to_string())]);

        let grouped = space_group_heads(
            AgentOrder::Quota,
            &Grouping::spaces(AgentOrder::Quota),
            &inventory,
            &inventory,
            &[],
        );
        assert_eq!(grouped.get("w1").map(String::as_str), Some("w1:p1"));
        let flat = space_group_heads(
            AgentOrder::Attention,
            &Grouping::spaces(AgentOrder::Attention),
            &inventory,
            &inventory,
            &[],
        );
        assert!(flat.is_empty());

        for pane in [&tight, &loose] {
            let mut desired = pane.tokens.clone();
            desired.insert("quota_group".to_string(), "玉衡".to_string());
            apply_group_and_icon(
                &mut desired,
                pane,
                &Grouping::spaces(crate::cli::AgentOrder::Quota),
                &flat,
                &labels,
                &Look::plain(),
            );
            assert!(!desired.contains_key("quota_group"));
            let icon = desired.get(pane.icon_status().icon_token()).unwrap();
            assert!(!icon.starts_with(GROUP_MEMBER_INDENT));
        }
    }

    #[test]
    fn switching_order_changes_no_quota_token() {
        // 0923 rollback lesson: a build change must not move a quota row. The
        // order may only move the Space header and the member indent.
        use crate::cli::AgentOrder;
        let pane = attention_pane("w1:p2", "090", AgentStatus::Idle);
        let other = attention_pane("w1:p1", "010", AgentStatus::Idle);
        let inventory = vec![other, pane.clone()];
        let labels = BTreeMap::from([("w1".to_string(), "玉衡".to_string())]);
        let render = |order| {
            let heads =
                space_group_heads(order, &Grouping::spaces(order), &inventory, &inventory, &[]);
            let mut desired = pane.tokens.clone();
            apply_group_and_icon(
                &mut desired,
                &pane,
                &Grouping::spaces(crate::cli::AgentOrder::Quota),
                &heads,
                &labels,
                &Look::plain(),
            );
            desired
        };
        let grouped = render(AgentOrder::Quota);
        let flat = render(AgentOrder::Attention);
        for name in METADATA_TOKEN_NAMES {
            if ICON_TOKEN_NAMES.contains(&name) || name == "quota_group" {
                continue;
            }
            assert_eq!(grouped.get(name), flat.get(name), "{name} moved");
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, label)| (id.to_string(), label.to_string()))
            .collect()
    }

    #[test]
    fn the_role_comes_from_the_label_and_the_model_from_the_session() {
        use crate::cli::AgentOrder;
        let mut pane = attention_pane("w1:p1", "050", AgentStatus::Idle);
        let panes = labels(&[("w1:p1", "hub.alioth.统筹.0925-2307.opus-5-5@cpu-box")]);
        // No session: the label's model segment is no evidence (owner 0927).
        let mut desired = BTreeMap::from([("quota_model".to_string(), "Fable 5.1".to_string())]);
        apply_seat_identity(
            &mut desired,
            &pane,
            Some(&panes),
            AgentOrder::Quota,
            &BTreeMap::new(),
        );
        assert_eq!(desired.get(ROLE_TOKEN).map(String::as_str), Some("统筹"));
        assert!(!desired.contains_key(MODEL_TOKEN));
        assert!(!desired.contains_key(SPACE_TOKEN));
        // With its own session, the reported model, shortened.
        pane.session = Some(AgentSession {
            kind: None,
            value: "s-1".to_string(),
        });
        let mut desired = BTreeMap::from([(
            "quota_model".to_string(),
            "Opus 5.5 (1M context)".to_string(),
        )]);
        apply_seat_identity(
            &mut desired,
            &pane,
            Some(&panes),
            AgentOrder::Quota,
            &BTreeMap::new(),
        );
        assert_eq!(
            desired.get(MODEL_TOKEN).map(String::as_str),
            Some("opus-5.5")
        );
    }

    #[test]
    fn the_model_is_never_taken_from_the_provider_model_token() {
        use crate::cli::AgentOrder;
        let mut pane = attention_pane("w1:p1", "050", AgentStatus::Idle);
        pane.tokens.insert(
            "quota_provider_model".to_string(),
            "Claude/Fable 5.1".to_string(),
        );
        // A label without a model segment reports no model at all.
        let panes = labels(&[("w1:p1", "spk.lab.起服.0926-0100@cpu-box")]);
        let mut desired = pane.tokens.clone();
        apply_seat_identity(
            &mut desired,
            &pane,
            Some(&panes),
            AgentOrder::Quota,
            &BTreeMap::new(),
        );
        assert_eq!(desired.get(ROLE_TOKEN).map(String::as_str), Some("spoke"));
        assert!(!desired.contains_key(MODEL_TOKEN));
    }

    #[test]
    fn a_failed_label_read_preserves_and_an_off_spec_label_clears() {
        use crate::cli::AgentOrder;
        let mut pane = attention_pane("w1:p1", "050", AgentStatus::Idle);
        pane.tokens
            .insert(ROLE_TOKEN.to_string(), "hub".to_string());
        pane.tokens
            .insert(MODEL_TOKEN.to_string(), "opus-5-5".to_string());
        let mut kept = BTreeMap::new();
        apply_seat_identity(&mut kept, &pane, None, AgentOrder::Quota, &BTreeMap::new());
        assert_eq!(kept.get(ROLE_TOKEN).map(String::as_str), Some("hub"));
        // The model name no longer rides on the label, so a failed label read
        // does not keep one either: without session evidence it is empty.
        assert!(!kept.contains_key(MODEL_TOKEN));

        let panes = labels(&[("w1:p1", "1")]);
        let mut cleared = pane.tokens.clone();
        apply_seat_identity(
            &mut cleared,
            &pane,
            Some(&panes),
            AgentOrder::Quota,
            &BTreeMap::new(),
        );
        assert!(!cleared.contains_key(ROLE_TOKEN));
        assert!(!cleared.contains_key(MODEL_TOKEN));
    }

    #[test]
    fn the_space_name_rides_on_each_seat_only_when_not_grouped() {
        use crate::cli::AgentOrder;
        let pane = attention_pane("w1:p1", "050", AgentStatus::Idle);
        let spaces = labels(&[("w1", "玉衡")]);
        let mut flat = BTreeMap::new();
        apply_seat_identity(
            &mut flat,
            &pane,
            Some(&BTreeMap::new()),
            AgentOrder::Attention,
            &spaces,
        );
        assert_eq!(flat.get(SPACE_TOKEN).map(String::as_str), Some("玉衡"));
        let mut grouped = flat.clone();
        apply_seat_identity(
            &mut grouped,
            &pane,
            Some(&BTreeMap::new()),
            AgentOrder::Quota,
            &spaces,
        );
        assert!(!grouped.contains_key(SPACE_TOKEN));
    }

    #[test]
    fn a_deepseek_seat_shows_the_whale_and_keeps_it_on_icon_only_passes() {
        let mut pane = attention_pane("w1:p1", "050", AgentStatus::Working);
        let mut desired = BTreeMap::from([(MODEL_TOKEN.to_string(), "ds-v4-1-flash".to_string())]);
        apply_group_and_icon(
            &mut desired,
            &pane,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &Look::plain(),
        );
        assert_eq!(
            desired.get("quota_icon_working").map(String::as_str),
            Some("\u{e1bc}")
        );
        // Focus / watcher passes start from the pane's tokens only.
        pane.tokens
            .insert(MODEL_TOKEN.to_string(), "ds-v4-1-flash".to_string());
        let mut icon_only = BTreeMap::new();
        apply_group_and_icon(
            &mut icon_only,
            &pane,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &Look::plain(),
        );
        assert_eq!(
            icon_only.get("quota_icon_working").map(String::as_str),
            Some("\u{e1bc}")
        );
        let mut letters = BTreeMap::new();
        apply_group_and_icon(
            &mut letters,
            &pane,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &Look {
                style: crate::icons::IconStyle::Letter,
                ..Look::plain()
            },
        );
        assert_eq!(
            letters.get("quota_icon_working").map(String::as_str),
            Some("DS")
        );
    }

    #[test]
    fn pane_labels_are_read_from_the_pane_list() {
        let value = serde_json::json!({"result": {"panes": [
            {"pane_id": "w1:p1", "workspace_id": "w1", "label": "hub.alioth.统筹.0925-2307.opus-5-5@cpu-box"},
            {"pane_id": "w1:p2", "workspace_id": "w1"}
        ]}});
        let mut found = BTreeMap::new();
        collect_pane_labels(&value, &mut found);
        assert_eq!(found.len(), 1);
        assert!(found["w1:p1"].starts_with("hub.alioth."));
    }

    #[test]
    fn a_cursor_pane_is_never_read_for_a_topic() {
        assert!(!reads_screen_topic(Harness::Cursor));
        assert!(reads_screen_topic(Harness::Claude));
        assert!(reads_screen_topic(Harness::Codex));
    }

    #[test]
    fn a_cursor_pane_is_listed_for_its_icon_but_bills_nothing() {
        let value = serde_json::json!({"result": {"agents": [
            {"agent": "cursor", "pane_id": "w3:p1", "agent_status": "idle"}
        ]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].harness, Harness::Cursor);
        assert_eq!(Harness::Cursor.billing(), None);
    }

    #[test]
    fn the_inventory_reads_back_state_labels() {
        let value = serde_json::json!({"result": {"agents": [{
            "agent": "claude",
            "pane_id": "w2:p1",
            "agent_status": "blocked",
            "state_labels": {
                "blocked": "[卡住]", "done": "[已回]", "working": "[工作]",
                "idle": "[空闲]", "unknown": "[未报]"
            },
            "tokens": {"quota_headroom": "090"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].status, AgentStatus::Blocked);
        assert!(state_labels_current(&panes[0].state_labels));
    }
    use crate::cli::{FieldSet, PercentStyle, SidebarLayout};
    use crate::model::{
        CacheUsage, ContextUsage, ProviderSnapshot, ResetAt, UsageWindow, WindowKind,
    };
    use serde_json::json;

    /// Herdr orders an Agent view by the token's own value, so the padding is
    /// the whole contract: `007` must sort before `042`, and `100` last.
    #[test]
    fn the_headroom_token_is_padded_so_its_text_order_is_its_numeric_order() {
        let token = |headroom: Option<u8>| {
            let mut values = MetadataTokens::unavailable(Provider::Claude, "test");
            values.quota_headroom = headroom;
            desired_tokens(&values, "", SidebarShape::default())
                .get(HEADROOM_TOKEN)
                .cloned()
        };
        assert_eq!(token(Some(7)).as_deref(), Some("007"));
        assert_eq!(token(Some(42)).as_deref(), Some("042"));
        assert_eq!(token(Some(100)).as_deref(), Some("100"));
        assert_eq!(token(None), None);

        let mut sorted = ["100", "007", "042", "000"];
        sorted.sort_unstable();
        assert_eq!(sorted, ["000", "007", "042", "100"]);
    }

    /// The comparison set and the report set are the same list, so a token
    /// that is published but not listed silently stops being compared and
    /// every refresh becomes a write.
    #[test]
    fn the_headroom_token_is_listed_among_the_names_that_are_compared() {
        assert!(METADATA_TOKEN_NAMES.contains(&HEADROOM_TOKEN));
        assert!(!OBSOLETE_METADATA_TOKEN_NAMES.contains(&HEADROOM_TOKEN));
    }

    /// A one-pane publish still knows the Space head from inventory, and the
    /// non-head drops `$quota_group` instead of preserving a stale label.
    #[test]
    fn group_header_lands_on_the_tightest_pane_and_clears_siblings() {
        let head = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([
                (HEADROOM_TOKEN.to_string(), "000".to_string()),
                (SORT_TOKEN.to_string(), "000w1:p1".to_string()),
                ("quota_group".to_string(), "ifs".to_string()),
            ]),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let sibling = AgentPane {
            pane_id: "w1:p2".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([
                (HEADROOM_TOKEN.to_string(), "016".to_string()),
                ("quota_group".to_string(), "ifs".to_string()),
            ]),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let inventory = vec![head.clone(), sibling.clone()];
        let heads = group_head_pane_ids(
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &inventory,
            std::slice::from_ref(&sibling),
            &[],
        );
        assert_eq!(heads.get("w1").map(String::as_str), Some("w1:p1"));

        let labels = BTreeMap::from([("w1".to_string(), "ifs".to_string())]);
        let mut head_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut head_desired,
            &head,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert_eq!(
            head_desired.get("quota_group").map(String::as_str),
            Some("ifs")
        );

        let mut sibling_desired = sibling.tokens.clone();
        apply_group_and_icon(
            &mut sibling_desired,
            &sibling,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(!sibling_desired.contains_key("quota_group"));
        assert!(
            sibling_desired
                .get("quota_icon")
                .is_some_and(|icon| icon.starts_with(GROUP_MEMBER_INDENT)),
            "member logo must carry the hang-indent: {:?}",
            sibling_desired.get("quota_icon")
        );
        assert!(
            !sibling_desired.contains_key("quota_pad"),
            "stand-alone pad draws a leading · separator"
        );
        assert_eq!(
            group_label_for(
                &sibling,
                &Grouping::spaces(crate::cli::AgentOrder::Quota),
                &heads,
                &labels
            ),
            None,
            "stale sibling header must clear"
        );
        assert!(
            head_desired
                .get("quota_icon")
                .is_some_and(|icon| !icon.starts_with('\u{200b}')),
            "head logo stays bare; Herdr hang-indents the row"
        );
        assert_eq!(
            format!("{GROUP_MEMBER_INDENT}x").trim(),
            format!("{GROUP_MEMBER_INDENT}x"),
            "indent glued to a glyph must survive Unicode trim"
        );

        // Brand icon colour follows agent_status: working publishes the
        // yellow twin and clears idle/done so only one glyph shows.
        let mut working = sibling.clone();
        working.status = AgentStatus::Working;
        let mut working_desired = BTreeMap::from([
            ("quota_icon".to_string(), "stale".to_string()),
            ("quota_icon_done".to_string(), "stale".to_string()),
        ]);
        apply_group_and_icon(
            &mut working_desired,
            &working,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(
            working_desired
                .get("quota_icon_working")
                .is_some_and(|icon| icon.starts_with(GROUP_MEMBER_INDENT)),
            "working panes publish the yellow brand icon"
        );
        assert!(!working_desired.contains_key("quota_icon"));
        assert!(!working_desired.contains_key("quota_icon_done"));

        let mut done = sibling.clone();
        done.status = AgentStatus::Done;
        let mut done_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut done_desired,
            &done,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(done_desired.contains_key("quota_icon_done"));
        assert!(!done_desired.contains_key("quota_icon"));
        assert!(!done_desired.contains_key("quota_icon_working"));

        // Merely being focused when a turn finishes does not acknowledge it.
        let mut seen = done.clone();
        seen.focused = true;
        let mut seen_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut seen_desired,
            &seen,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(
            seen_desired.contains_key("quota_icon_done"),
            "focused completion stays teal until the focus hook acknowledges it"
        );
        assert!(!seen_desired.contains_key("quota_icon"));

        // Unfocused sibling finishing must keep teal — not follow the focused
        // pane's yellow→white shortcut.
        let mut other = sibling.clone();
        other.pane_id = "w1:p3".into();
        other.status = AgentStatus::Done;
        other.focused = false;
        let mut other_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut other_desired,
            &other,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(
            other_desired.contains_key("quota_icon_done"),
            "unfocused completion keeps teal until that pane is focused"
        );
        assert!(!other_desired.contains_key("quota_icon"));

        // Same-tab: refresh folds unseen into status=Done before publish.
        // A leftover done token on idle must not paint teal by itself —
        // that is how a stale agent-list read restored teal after focus.
        let mut stale_token = other.clone();
        stale_token.status = AgentStatus::Idle;
        stale_token
            .tokens
            .insert("quota_icon_done".to_string(), "teal".to_string());
        let mut stale_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut stale_desired,
            &stale_token,
            &Grouping::spaces(crate::cli::AgentOrder::Quota),
            &heads,
            &labels,
            &Look::plain(),
        );
        assert!(
            stale_desired.contains_key("quota_icon"),
            "idle + leftover done token must not restore teal"
        );
        assert!(!stale_desired.contains_key("quota_icon_done"));
    }

    #[test]
    fn discovers_canonical_agent_panes_from_nested_json() {
        let value = json!({"result": {"agents": [
            {"pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "codex"},
            {"pane_id": "w1:p2", "tab_id": "w1:t2", "agent_session": {"agent": "claude"}},
            {"pane_id": "w1:p3", "agent": "unknown"}
        ], "tabs": [
            {"tab_id": "w1:t1", "label": "Owner"},
            {"tab_id": "w1:t2", "label": "Executor"}
        ]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        assert_eq!(
            panes,
            vec![
                AgentPane {
                    pane_id: "w1:p1".to_string(),
                    workspace_id: "w1".to_string(),
                    harness: Harness::Codex,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                    status: AgentStatus::Idle,
                    focused: false,
                    state_labels: BTreeMap::new(),
                },
                AgentPane {
                    pane_id: "w1:p2".to_string(),
                    workspace_id: "w1".to_string(),
                    harness: Harness::Claude,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                    status: AgentStatus::Idle,
                    focused: false,
                    state_labels: BTreeMap::new(),
                },
            ]
        );
    }

    /// Herdr reports at most 16 metadata tokens per pane. A snapshot that
    /// fills every optional slot must still fit, or the tail is silently
    /// dropped and the sidebar loses whichever rows land last.
    #[test]
    fn a_fully_populated_pane_stays_within_herdrs_sixteen_token_report_cap() {
        const HERDR_TOKEN_REPORT_CAP: usize = 16;
        for provider in [Provider::Codex, Provider::Claude] {
            let snapshot = ProviderSnapshot::new(
                provider,
                vec![
                    UsageWindow::new(
                        WindowKind::FiveHour,
                        85.0,
                        Some(ResetAt::from_unix_seconds(9_000)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Weekly,
                        42.0,
                        Some(ResetAt::from_unix_seconds(600_000)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Monthly,
                        10.0,
                        Some(ResetAt::from_unix_seconds(2_000_000)),
                    )
                    .unwrap(),
                ],
                0,
            )
            .with_model(Some("A Very Long Model Name".to_string()))
            .with_context(Some(ContextUsage {
                used_percent: 61.0,
                cache: Some(CacheUsage {
                    fresh_input_tokens: 1_000,
                    read_tokens: 50_000,
                    creation_tokens: 2_000,
                    hit_percent: 96.4,
                    ttl_seconds: Some(3_540),
                    last_activity_unix: None,
                    expires_at_unix: None,
                    session_totals: None,
                    session_id: None,
                    transcript_offset: 0,
                }),
            }));
            let desired = desired_tokens(
                &MetadataTokens::from_snapshot(&snapshot, 0),
                "a topic that is present",
                SidebarShape::default(),
            );
            assert!(
                desired.len() <= HERDR_TOKEN_REPORT_CAP,
                "{provider:?} would report {} tokens: {:?}",
                desired.len(),
                desired.keys().collect::<Vec<_>>()
            );
            // A monthly window has its own token; it must not ride a weekly one.
            for (name, value) in &desired {
                if name.starts_with("quota_month") {
                    continue;
                }
                assert!(
                    !value.contains("30d"),
                    "{provider:?} put a monthly value in {name}"
                );
            }
        }
    }

    /// 0927 (Codex review): with identity rows, logos, a counted header and
    /// a gap, a fully populated pane has more names than one report may
    /// carry. The ones that keep the view and its heads consistent, and the
    /// rows the identity sidebar actually shows, must never be the ones
    /// deferred; hidden quota values may lag a pass instead.
    #[test]
    fn a_full_pane_never_defers_the_sort_key_header_or_logo_marks() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                UsageWindow::new(
                    WindowKind::FiveHour,
                    85.0,
                    Some(ResetAt::from_unix_seconds(9_000)),
                )
                .unwrap(),
                UsageWindow::new(
                    WindowKind::Weekly,
                    42.0,
                    Some(ResetAt::from_unix_seconds(600_000)),
                )
                .unwrap(),
            ],
            0,
        )
        .with_model(Some("Opus 5.5".to_string()))
        .with_context(Some(ContextUsage {
            used_percent: 61.0,
            cache: Some(CacheUsage {
                fresh_input_tokens: 1_000,
                read_tokens: 50_000,
                creation_tokens: 2_000,
                hit_percent: 96.4,
                ttl_seconds: Some(3_540),
                last_activity_unix: None,
                expires_at_unix: None,
                session_totals: None,
                session_id: None,
                transcript_offset: 0,
            }),
        }));
        let mut desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "a topic that is present",
            SidebarShape::default(),
        );
        for name in [
            "quota_group",
            "quota_icon",
            ROLE_TOKEN,
            MODEL_TOKEN,
            SPACE_TOKEN,
            SORT_TOKEN,
            GAP_TOKEN,
            CHANNEL_TOKEN,
            MODEL_MARK_TOKEN,
            GROUP_TOKEN,
            ROLE_RANK_TOKEN,
            ATTENTION_TOKEN,
        ] {
            desired.insert(name.to_string(), "x".to_string());
        }
        let mut pane = attention_pane("w1:p1", "010", AgentStatus::Idle);
        // Last pass the pane was working: the working twin must be cleared.
        pane.tokens = desired.clone();
        pane.tokens.remove("quota_icon");
        pane.tokens
            .insert("quota_icon_working".to_string(), "x".to_string());
        pane.tokens
            .insert(SORT_TOKEN.to_string(), "old".to_string());
        let quota_rows = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(quota_rows.len() <= MAX_METADATA_TOKENS, "{quota_rows:?}");
        for name in ["quota_group", SORT_TOKEN, "quota_icon_working"] {
            assert!(quota_rows.contains(&name), "quota rows deferred {name}");
        }
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Identity);
        assert!(names.len() <= MAX_METADATA_TOKENS, "{names:?}");
        for name in [
            "quota_group",
            "quota_icon",
            "quota_icon_working",
            "quota_icon_done",
            SORT_TOKEN,
            GAP_TOKEN,
            CHANNEL_TOKEN,
            MODEL_MARK_TOKEN,
            ROLE_TOKEN,
            MODEL_TOKEN,
            SPACE_TOKEN,
            GROUP_TOKEN,
            ROLE_RANK_TOKEN,
            ATTENTION_TOKEN,
        ] {
            assert!(names.contains(&name), "{name} deferred: {names:?}");
        }
    }

    #[test]
    fn plugin_quota_presence_ignores_topic_only_tokens() {
        let mut tokens = BTreeMap::new();
        tokens.insert("quota_topic".to_string(), "keep me".to_string());
        assert!(!plugin_quota_present(&tokens));
        tokens.insert("quota_5h".to_string(), "5h 10%".to_string());
        assert!(plugin_quota_present(&tokens));
    }

    #[test]
    fn carries_path_kind_without_exposing_it_as_an_id() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p9",
            "agent": "codex",
            "agent_session": {
                "agent": "codex",
                "kind": "path",
                "source": "herdr:codex",
                "value": "/tmp/sessions/project/session-transcript.jsonl"
            }
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        let session = panes[0].session.as_ref().unwrap();
        assert_eq!(panes[0].harness, Harness::Codex);
        assert_eq!(session.kind.as_deref(), Some("path"));
        assert_eq!(session.id(), None);
        assert_eq!(
            session.path(),
            Some("/tmp/sessions/project/session-transcript.jsonl")
        );
    }

    #[test]
    fn id_kind_preserves_every_existing_harness_session() {
        for agent in ["claude", "codex"] {
            let value = json!({"result": {"agents": [{
                "pane_id": "w1:p1",
                "agent": agent,
                "agent_session": {"kind": "id", "value": "session-id"}
            }]}});
            let mut panes = Vec::new();
            collect_agent_panes(&value, &mut panes);
            assert_eq!(
                panes[0].session.as_ref().and_then(AgentSession::id),
                Some("session-id"),
                "{agent}"
            );
            assert_eq!(
                panes[0].session.as_ref().and_then(AgentSession::path),
                None,
                "{agent}"
            );
        }
    }

    #[test]
    fn unknown_session_kinds_are_not_reinterpreted() {
        for kind in ["PATH", "ID", "uri"] {
            let session = AgentSession {
                kind: Some(kind.to_string()),
                value: "session-value".to_string(),
            };
            assert_eq!(session.id(), None, "{kind}");
            assert_eq!(session.path(), None, "{kind}");
        }
    }

    #[test]
    fn quota_only_discovery_preserves_the_last_published_topic() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p1",
            "agent": "codex",
            "tokens": {"quota_topic": "latest task"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].topic, "latest task");
    }

    #[test]
    fn discovers_codex_session_id_and_preserves_session_summary() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p1",
            "agent": "codex",
            "agent_session": {"agent": "codex", "value": "thread-1"},
            "tokens": {"quota_session": "previous summary"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("thread-1")
        );
        assert_eq!(panes[0].session_summary, "previous summary");
    }

    #[test]
    fn legacy_metadata_tokens_force_one_bounded_cleanup_report() {
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([(String::from("quota_badge"), String::from("[A]"))]),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let desired = BTreeMap::from([(String::from("quota_state"), String::from("?"))]);
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.contains(&"quota_badge"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn weekly_only_inline_week_stays_inside_herdr_metadata_cap() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                30.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(!names.contains(&"quota_week_normal"));
        assert!(!names.contains(&"quota_5h"));
    }

    #[test]
    fn cache_diagnostics_stay_inside_herdr_metadata_cap() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(crate::model::WindowKind::FiveHour, 20.0, None)
                    .unwrap(),
                crate::model::UsageWindow::new(crate::model::WindowKind::Weekly, 30.0, None)
                    .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
    }

    /// The context row is coloured by the context *left* under `gauges`, on
    /// the windows' own bands, whichever side of the ledger it prints.
    #[test]
    fn gauges_publishes_context_into_the_severity_name_its_headroom_earns() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        for (used, expected) in [
            (31.0, "quota_context_normal"),
            (49.0, "quota_context_normal"),
            (50.0, "quota_context_normal"),
            (51.0, "quota_context_warning"),
            (53.0, "quota_context_warning"),
            (79.0, "quota_context_warning"),
            (80.0, "quota_context_warning"),
            (81.0, "quota_context_danger"),
            (85.0, "quota_context_danger"),
        ] {
            for percent in [PercentStyle::Remaining, PercentStyle::Used] {
                let mut tokens = BTreeMap::new();
                apply_context(
                    &mut tokens,
                    &ContextUsage::new(used).unwrap(),
                    0,
                    RowStyle::new(percent, gauges),
                );
                let published = CONTEXT_TOKEN_NAMES
                    .into_iter()
                    .filter(|name| tokens.contains_key(*name))
                    .collect::<Vec<_>>();
                assert_eq!(
                    published,
                    vec![expected],
                    "context {used} used, {percent:?}"
                );
            }
        }
    }

    /// Only one context name is ever filled, so a severity change or a layout
    /// switch can never leave a pane showing two context rows.
    #[test]
    fn a_context_severity_change_clears_the_name_it_moved_away_from() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        let mut tokens = BTreeMap::new();
        let row = RowStyle::new(PercentStyle::Used, gauges);
        apply_context(&mut tokens, &ContextUsage::new(31.0).unwrap(), 0, row);
        apply_context(&mut tokens, &ContextUsage::new(85.0).unwrap(), 0, row);
        assert!(!tokens.contains_key("quota_context_normal"));
        assert_eq!(
            tokens.get("quota_context_danger").map(String::as_str),
            Some("cx 85%")
        );
        apply_context(
            &mut tokens,
            &ContextUsage::new(85.0).unwrap(),
            0,
            RowStyle::default(),
        );
        assert_eq!(
            tokens.get("quota_context").map(String::as_str),
            Some("context 85%")
        );
        for name in ["quota_context_normal", "quota_context_danger"] {
            assert!(!tokens.contains_key(name), "{name}");
        }
    }

    /// Related cache details share a line when both fields are visible and
    /// the joined text fits the content width; they split again when it does
    /// not. Packed and stacked never fold, and hiding either field keeps the
    /// two tokens apart so the empty Herdr row can still collapse.
    #[test]
    fn gauges_join_cache_and_ttl_when_they_fit_the_content_width() {
        let cache_ttl = || {
            BTreeMap::from([
                ("quota_cache".to_string(), "cache 95.2%".to_string()),
                ("quota_cache_ttl".to_string(), "ttl≈29m".to_string()),
            ])
        };
        let mut wide = cache_ttl();
        fold_cache_row(
            &mut wide,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 26),
            },
        );
        assert_eq!(
            wide.get("quota_cache").map(String::as_str),
            Some("cache 95.2% · ttl≈29m")
        );
        assert!(!wide.contains_key("quota_cache_ttl"));

        let mut narrow = cache_ttl();
        fold_cache_row(
            &mut narrow,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 18),
            },
        );
        assert_eq!(
            narrow.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert_eq!(
            narrow.get("quota_cache_ttl").map(String::as_str),
            Some("ttl≈29m")
        );

        let mut cache_only = cache_ttl();
        fold_cache_row(
            &mut cache_only,
            RowStyle {
                fields: FieldSet::parse("cache").unwrap(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 26),
            },
        );
        assert_eq!(
            cache_only.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert!(cache_only.contains_key("quota_cache_ttl"));

        let mut packed = cache_ttl();
        fold_cache_row(
            &mut packed,
            RowStyle::new(
                PercentStyle::Remaining,
                SidebarShape::from(SidebarLayout::Packed),
            ),
        );
        assert_eq!(
            packed.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert!(packed.contains_key("quota_cache_ttl"));
    }

    /// `no cached` stays on `$quota_cache_state` even when the joined text
    /// would fit. Concatenating it into `$quota_cache` would share the line
    /// and lose the amber warning; gauges puts the two tokens on one row.
    #[test]
    fn gauges_do_not_fold_no_cached_into_the_cache_token() {
        let mut tokens = BTreeMap::from([
            ("quota_cache".to_string(), "cache 70.7%".to_string()),
            ("quota_cache_state".to_string(), "no cached".to_string()),
        ]);
        fold_cache_row(
            &mut tokens,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 36),
            },
        );
        assert_eq!(
            tokens.get("quota_cache").map(String::as_str),
            Some("cache 70.7%")
        );
        assert_eq!(
            tokens.get("quota_cache_state").map(String::as_str),
            Some("no cached")
        );
    }

    /// `packed` and `stacked` keep the plain uncoloured name they have always
    /// published, and the used percent they have always printed, whatever the
    /// context value and whichever percent style the windows are drawn with.
    #[test]
    fn packed_and_stacked_keep_publishing_the_plain_context_token() {
        for layout in [
            crate::cli::SidebarLayout::Packed,
            crate::cli::SidebarLayout::Stacked,
        ] {
            for percent in [PercentStyle::Remaining, PercentStyle::Used] {
                let mut tokens = BTreeMap::new();
                apply_context(
                    &mut tokens,
                    &ContextUsage::new(85.0).unwrap(),
                    0,
                    RowStyle::new(percent, SidebarShape::from(layout)),
                );
                assert_eq!(
                    tokens.get("quota_context").map(String::as_str),
                    Some("context 85%"),
                    "{layout:?} {percent:?}"
                );
                for name in [
                    "quota_context_normal",
                    "quota_context_warning",
                    "quota_context_danger",
                ] {
                    assert!(!tokens.contains_key(name), "{layout:?} wrote {name}");
                }
            }
        }
    }

    /// The three context names are three more slots against Herdr's sixteen,
    /// even though at most one of them is ever filled.
    #[test]
    fn a_full_gauges_pane_stays_inside_herdr_metadata_cap() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    20.0,
                    Some(crate::model::ResetAt::from_unix_seconds(18_000)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    30.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(85.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            crate::cli::PercentStyle::default(),
            gauges,
        );
        let desired = desired_tokens(&values, "prompt", gauges);
        assert!(desired.contains_key("quota_context_danger"));
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.len() <= MAX_METADATA_TOKENS, "{names:?}");
    }

    #[test]
    fn exact_context_without_cache_clears_stale_cache_diagnostics() {
        let mut tokens = BTreeMap::from([
            ("quota_context".to_string(), "context 99%".to_string()),
            ("quota_cache".to_string(), "cache 95.0%".to_string()),
            ("quota_cache_ttl".to_string(), "ttl≈1h".to_string()),
        ]);
        apply_context(
            &mut tokens,
            &ContextUsage::new(12.0).unwrap(),
            0,
            RowStyle::default(),
        );
        assert_eq!(
            tokens.get("quota_context").map(String::as_str),
            Some("context 12%")
        );
        assert!(!tokens.contains_key("quota_cache"));
        assert!(!tokens.contains_key("quota_cache_ttl"));
    }

    #[test]
    fn stale_metadata_tokens_are_reported_for_cleanup_with_new_cache_rows() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(crate::model::WindowKind::FiveHour, 20.0, None)
                    .unwrap(),
                crate::model::UsageWindow::new(crate::model::WindowKind::Weekly, 30.0, None)
                    .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.insert("quota_summary".to_string(), "old".to_string());
        tokens.insert("quota_status".to_string(), "OK".to_string());
        tokens.insert("quota_badge".to_string(), "[C]".to_string());
        tokens.insert("quota_session".to_string(), "old".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
        assert!(names.contains(&"quota_summary"));
        assert!(names.contains(&"quota_status"));
        assert!(names.contains(&"quota_badge"));
        assert!(names.contains(&"quota_session"));
    }

    #[test]
    fn working_agent_detection_handles_herdr_agent_list_shape() {
        let value = json!({"result": {"agents": [
            {"agent": "claude", "agent_status": "working"},
            {"agent": "codex", "agent_status": "idle"},
            {"agent": "claude", "agent_status": "working"}
        ]}});
        assert_eq!(working_providers_from(&value), vec![Provider::Claude]);
    }

    #[test]
    fn one_agent_inventory_deduplicates_working_providers() {
        let value = json!({"result": {"agents": [
            {"agent": "codex", "agent_status": "working"},
            {"agent_session": {"agent": "codex"}, "status": "working"},
            {"agent": "claude", "agent_status": "idle"}
        ]}});
        assert_eq!(working_providers_from(&value), vec![Provider::Codex]);
    }

    #[test]
    fn extracts_latest_claude_prompt_and_skips_clear_command() {
        let text = "❯ /clear\n❯ hi\n⏺ Hi! What can I help with?\n❯\n";
        assert_eq!(extract_topic(text, Harness::Claude).as_deref(), Some("hi"));
    }

    #[test]
    fn ignores_codex_default_prompt_placeholder() {
        assert_eq!(
            extract_topic("› Ask Codex to do anything\n", Harness::Codex),
            None
        );
    }

    #[test]
    fn ignores_ai_status_title_as_a_topic() {
        let value = json!({
            "pane_id": "w1:p1",
            "agent": "codex",
            "terminal_title_stripped": "Thinking - L7 Learning Reset"
        });
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].topic, "");
    }

    #[test]
    fn a_missing_five_hour_window_folds_week_onto_context() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                31.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        assert!(!desired.contains_key("quota_5h_unknown"));
        assert!(!desired.contains_key("quota_5h_normal"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_normal"));
    }

    #[test]
    fn empty_five_hour_publishes_week_beside_context() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                31.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        assert!(!desired.contains_key("quota_5h_normal"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_inline_label"));
        assert!(!desired.contains_key("quota_week_normal"));
    }

    #[test]
    fn present_five_hour_keeps_week_off_the_context_row() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    5.0,
                    Some(crate::model::ResetAt::from_unix_seconds(14_820)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    1.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        );
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        assert_eq!(
            desired.get("quota_5h_normal").map(String::as_str),
            Some("5h 95% 4h07m")
        );
        assert_eq!(
            desired.get("quota_week_normal").map(String::as_str),
            Some("7d 99% 2d3h")
        );
        assert!(!desired.contains_key("quota_5h"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(!desired.contains_key("quota_5h_eta"));
        assert!(!desired.contains_key("quota_week"));
        assert!(!desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_inline_warning"));
        assert!(!desired.contains_key("quota_week_inline_danger"));
    }

    #[test]
    fn folding_week_onto_context_clears_limits_week_styles() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                25.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.remove("quota_week_inline_normal");
        tokens.insert("quota_week_normal".to_string(), "7d 75% 5d0h".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.contains(&"quota_week_normal"));
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_normal"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn switching_into_a_five_hour_window_clears_inline_week() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    5.0,
                    Some(crate::model::ResetAt::from_unix_seconds(14_820)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    1.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        );
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.insert("quota_week_inline_normal".to_string(), "7d 99%".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
            state_labels: BTreeMap::new(),
        };
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired, crate::cli::SidebarRows::Quota);
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(names.contains(&"quota_week_normal"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn publishes_exactly_one_styled_variant_for_each_window() {
        let mut tokens = BTreeMap::new();
        insert_severity_token(
            &mut tokens,
            "quota_week",
            "25%",
            Some(crate::model::Severity::Warning),
        );
        assert_eq!(
            tokens.get("quota_week_warning").map(String::as_str),
            Some("25%")
        );
        assert!(!tokens.contains_key("quota_week_normal"));
        assert!(!tokens.contains_key("quota_week_caution"));
        assert!(!tokens.contains_key("quota_week_danger"));
    }

    #[test]
    fn extracts_the_latest_codex_user_prompt_instead_of_ai_output() {
        let text = "❯ /goal 你在 ti 工作区接手 L7\n先读计划与权威文档，再按七步做 L7 盘点与设计。\n◇ Ran 1 subagent\n计划已读。先冻结坐标并读材料。\n";
        assert_eq!(
            extract_topic(text, Harness::Claude).as_deref(),
            Some("/goal 你在 ti 工作区接手 L7")
        );
    }

    // `recent` and `recent-unwrapped` rebuild the pane's wrapped scrollback,
    // which repaints it: one read, one visible scroll for the user.
    #[test]
    fn topic_reads_never_rebuild_a_pane_scrollback() {
        let args = topic_read_args("w1:p1");
        assert!(args.contains(&"visible"));
        assert!(!args.contains(&"recent"));
        assert!(!args.contains(&"recent-unwrapped"));
    }

    #[test]
    fn truncates_topics_without_splitting_utf8() {
        let topic = truncate_topic(&"你好".repeat(50));
        assert!(topic.ends_with('…'));
        assert!(topic.chars().count() <= 78);
    }
}

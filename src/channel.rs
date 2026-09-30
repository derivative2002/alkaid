//! Which channel (天枢 gateway or official) each pane's agent uses.
//!
//! Read from the agent process bound to the pane: the process inherits its
//! pane's `HERDR_PANE_ID`, and its `HERDR_SOCKET_PATH` says which Herdr
//! server the pane belongs to. The evidence is four non-secret entries —
//! `ANTHROPIC_BASE_URL` (reduced at once to a route, see
//! [`crate::look::route_of`]), `HOME`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME`.
//! The three directories are kept as paths: they locate the seat's own
//! transcript or rollout ([`crate::seat_model`]) and say whether the seat is
//! a launcher-registered lane. From the command line only a Codex
//! `resume <id>` session id is kept. The raw environ and argv blocks are held
//! in memory only while they are filtered; no other entry or argument is
//! decoded, kept, copied or logged — both can hold credentials.
//!
//! Scanned once per publish pass. Without `/proc` (macOS), or when the
//! process is unreadable, the channel is left blank and the model unread —
//! never inferred from the label or a model name.

use crate::look::Channel;
use crate::model::Harness;
use std::collections::BTreeMap;

/// What one pane's agent process revealed: a route, three directory paths
/// and a resume id. No other value from the process is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessChannel {
    /// `ANTHROPIC_BASE_URL` as a route; `None` when the variable is unset
    /// or has no host.
    pub route: Option<String>,
    /// `HOME` is an official Claude lane's isolated home
    /// (`start-cc-official*.sh` export `HOME=…/dot-claude-official[-N]`).
    pub official_home: bool,
    /// `CODEX_HOME` is one `start-codex-hub.sh` registers to the official
    /// egress (`codex-home`, `codex-home-2`, `codex-home-gpu`); the
    /// launcher refuses any other.
    pub registered_codex_home: bool,
    /// Where the seat keeps its own session files.
    pub paths: SeatPaths,
}

/// The agent process's own directories, as set in its environment, and the
/// session a Codex process was resumed with. Paths only — no file content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatPaths {
    pub home: Option<String>,
    pub claude_config_dir: Option<String>,
    pub codex_home: Option<String>,
    /// `codex … resume <id>`: the session a Codex pane runs when Herdr has
    /// not reported one.
    pub resume_session: Option<String>,
}

/// Agent process names worth reading. Claude Code runs as `claude` (native)
/// or `node`; Codex as `codex` (under a `node` launcher); Cursor Agent's node
/// renames its main thread `MainThread`.
const AGENT_COMMS: [&str; 4] = ["claude", "node", "codex", "MainThread"];

/// Pane id → the first agent process found in it, among processes of the
/// Herdr server this plugin runs under.
///
/// Pane ids are only unique per server: a sandbox or a second session on the
/// same machine has its own `w1:p3`. A process whose `HERDR_SOCKET_PATH`
/// names another server is skipped; when this process has no socket (a bare
/// CLI run) nothing can be attributed, so nothing is returned.
pub fn scan() -> BTreeMap<String, ProcessChannel> {
    let Some(own_socket) = std::env::var_os("HERDR_SOCKET_PATH") else {
        return BTreeMap::new();
    };
    scan_for(own_socket.to_string_lossy().as_ref())
}

fn scan_for(own_socket: &str) -> BTreeMap<String, ProcessChannel> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    let mut pids = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .collect::<Vec<_>>();
    // Oldest first: the agent itself precedes the helpers it spawns.
    pids.sort_unstable();
    for pid in pids {
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        if !AGENT_COMMS.contains(&comm.trim()) {
            continue;
        }
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let entries = read_entries(&environ);
        if entries.socket.as_deref() != Some(own_socket) {
            continue;
        }
        let evidence = ProcessChannel {
            route: entries.base_url.as_deref().and_then(crate::look::route_of),
            official_home: entries.home.as_deref().is_some_and(is_official_home),
            registered_codex_home: entries
                .codex_home
                .as_deref()
                .is_some_and(is_registered_codex_home),
            paths: SeatPaths {
                resume_session: std::fs::read(format!("/proc/{pid}/cmdline"))
                    .ok()
                    .and_then(|argv| resume_session(&argv)),
                home: entries.home,
                claude_config_dir: entries.claude_config_dir,
                codex_home: entries.codex_home,
            },
        };
        if let Some(pane) = entries.pane {
            found.entry(pane).or_insert(evidence);
        }
    }
    found
}

#[derive(Debug, Default)]
struct Entries {
    pane: Option<String>,
    socket: Option<String>,
    base_url: Option<String>,
    home: Option<String>,
    claude_config_dir: Option<String>,
    codex_home: Option<String>,
}

/// The session id after `resume` in a NUL-separated argv, when it looks like
/// an id (hex and dashes). Arguments are compared as bytes; only that id is
/// decoded and kept.
fn resume_session(argv: &[u8]) -> Option<String> {
    let mut args = argv.split(|byte| *byte == 0);
    args.find(|arg| *arg == b"resume")?;
    let id = args.next()?;
    (id.len() >= 8 && id.iter().all(|b| b.is_ascii_hexdigit() || *b == b'-'))
        .then(|| String::from_utf8_lossy(id).into_owned())
}

/// The six entries named above from an environ block; every other entry is
/// skipped without being decoded.
fn read_entries(environ: &[u8]) -> Entries {
    let mut found = Entries::default();
    for entry in environ.split(|byte| *byte == 0) {
        let text = |value: &[u8]| std::str::from_utf8(value).ok().map(str::to_string);
        if let Some(value) = entry.strip_prefix(b"HERDR_PANE_ID=") {
            found.pane = text(value).filter(|pane| !pane.is_empty());
        } else if let Some(value) = entry.strip_prefix(b"HERDR_SOCKET_PATH=") {
            found.socket = text(value);
        } else if let Some(value) = entry.strip_prefix(b"ANTHROPIC_BASE_URL=") {
            found.base_url = text(value);
        } else if let Some(value) = entry.strip_prefix(b"HOME=") {
            found.home = text(value);
        } else if let Some(value) = entry.strip_prefix(b"CLAUDE_CONFIG_DIR=") {
            found.claude_config_dir = text(value).filter(|dir| !dir.is_empty());
        } else if let Some(value) = entry.strip_prefix(b"CODEX_HOME=") {
            found.codex_home = text(value);
        }
    }
    found
}

/// Last path component, ignoring a trailing slash.
fn directory_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

/// `dot-claude-official` (lane 1) or `dot-claude-official-<n>`.
fn is_official_home(home: &str) -> bool {
    let name = directory_name(home);
    name == "dot-claude-official"
        || name
            .strip_prefix("dot-claude-official-")
            .is_some_and(|lane| !lane.is_empty() && lane.bytes().all(|b| b.is_ascii_digit()))
}

fn is_registered_codex_home(codex_home: &str) -> bool {
    matches!(
        directory_name(codex_home),
        "codex-home" | "codex-home-2" | "codex-home-gpu"
    )
}

/// The channel to show, or `None` to show nothing. Evidence only; every
/// row not listed is blank.
///
/// | Harness | 天枢 | Official |
/// |---|---|---|
/// | Claude Code | base-URL route on :8686 or a `dubhe` gateway path | no base URL, or `api.anthropic.com`, **and** an official-lane `HOME` |
/// | Codex | — (no gateway route is read) | a launcher-registered `CODEX_HOME` |
/// | Cursor | — | — (its endpoint is not read) |
///
/// An unset base URL alone does not prove official: an official lane also
/// goes through its bound egress, which only the launcher's `HOME` shows.
/// A Codex command-line `model_provider` override is not read (argv can
/// carry secrets), so a registered home is taken as the launcher's route.
pub fn classify(harness: Harness, process: Option<&ProcessChannel>) -> Option<Channel> {
    let process = process?;
    match harness {
        Harness::Claude => match process.route.as_deref() {
            Some(route) => match crate::look::channel_of_route(route)? {
                Channel::Dubhe => Some(Channel::Dubhe),
                Channel::Official => process.official_home.then_some(Channel::Official),
            },
            None => process.official_home.then_some(Channel::Official),
        },
        Harness::Codex => process.registered_codex_home.then_some(Channel::Official),
        Harness::Cursor => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_six_named_entries_are_taken() {
        let environ = b"PATH=/usr/bin\0ANTHROPIC_AUTH_TOKEN=sk-secret\0HERDR_PANE_ID=w2:p1\0HERDR_SOCKET_PATH=/run/a.sock\0ANTHROPIC_BASE_URL=http://u:p@127.0.0.1:8686/?k=1\0HOME=/v/dot-claude-official-2\0CLAUDE_CONFIG_DIR=/v/cfg\0CODEX_HOME=/v/codex-home\0";
        let found = read_entries(environ);
        assert_eq!(found.claude_config_dir.as_deref(), Some("/v/cfg"));
        assert_eq!(found.pane.as_deref(), Some("w2:p1"));
        assert_eq!(found.socket.as_deref(), Some("/run/a.sock"));
        assert_eq!(found.home.as_deref(), Some("/v/dot-claude-official-2"));
        assert_eq!(found.codex_home.as_deref(), Some("/v/codex-home"));
        // The URL is reduced to its route before anything keeps it.
        assert_eq!(
            found
                .base_url
                .as_deref()
                .and_then(crate::look::route_of)
                .as_deref(),
            Some("127.0.0.1:8686")
        );
        let found = read_entries(b"HERDR_PANE_ID=\0FOO=1\0");
        assert_eq!(found.pane, None);
        assert_eq!(found.base_url, None);
    }

    #[test]
    fn only_a_resume_session_id_is_taken_from_argv() {
        let argv = b"node\0/bin/codex\0--profile\0subhub\0-c\0model=gpt-6-astra\0resume\001a0cce2-646d-7c41-9139-d72199f76bda\0";
        assert_eq!(
            resume_session(argv).as_deref(),
            Some("01a0cce2-646d-7c41-9139-d72199f76bda")
        );
        assert_eq!(resume_session(b"codex\0--profile\0hub\0"), None);
        assert_eq!(resume_session(b"codex\0resume\0--last\0"), None);
        assert_eq!(resume_session(b"codex\0resume\0"), None);
    }

    #[test]
    fn launcher_directories_are_matched_by_name() {
        for home in [
            "/volume/yjiao/claude-code/dot-claude-official",
            "/volume/yjiao/claude-code/dot-claude-official-3/",
        ] {
            assert!(is_official_home(home), "{home}");
        }
        for home in [
            "/root",
            "/v/dot-claude-official-x",
            "/v/dot-claude-official-",
            "/v/dot-claude",
        ] {
            assert!(!is_official_home(home), "{home}");
        }
        assert!(is_registered_codex_home("/volume/yjiao/codex-home-gpu"));
        assert!(!is_registered_codex_home("/tmp/sb/codex"));
    }

    #[test]
    fn a_process_of_another_herdr_server_is_never_attributed() {
        // No live agent process carries this made-up socket, so nothing may
        // be found for it — a scan that ignored the socket would still
        // attribute any agent process on this machine to its pane id.
        assert!(scan_for("/nonexistent/herdr-test.sock").is_empty());
    }

    #[test]
    fn a_channel_needs_evidence_and_unknown_stays_blank() {
        let seat = |route: Option<&str>, official_home, registered_codex_home| ProcessChannel {
            route: route.map(str::to_string),
            official_home,
            registered_codex_home,
            paths: SeatPaths::default(),
        };
        let cases = [
            // Claude Code: a gateway route decides on its own.
            (
                Harness::Claude,
                Some(seat(Some("127.0.0.1:8686"), false, false)),
                Some(Channel::Dubhe),
            ),
            (
                Harness::Claude,
                Some(seat(Some("127.0.0.1:8686"), true, false)),
                Some(Channel::Dubhe),
            ),
            // Official needs the lane's HOME as well.
            (
                Harness::Claude,
                Some(seat(None, true, false)),
                Some(Channel::Official),
            ),
            (
                Harness::Claude,
                Some(seat(Some("api.anthropic.com"), true, false)),
                Some(Channel::Official),
            ),
            (Harness::Claude, Some(seat(None, false, false)), None),
            (
                Harness::Claude,
                Some(seat(Some("api.anthropic.com"), false, false)),
                None,
            ),
            (
                Harness::Claude,
                Some(seat(Some("example.net/v1"), true, false)),
                None,
            ),
            // Codex: the registered home, nothing else.
            (
                Harness::Codex,
                Some(seat(None, false, true)),
                Some(Channel::Official),
            ),
            (Harness::Codex, Some(seat(None, true, false)), None),
            (Harness::Cursor, Some(seat(None, true, true)), None),
            // No process read: blank for every harness, whatever the label says.
            (Harness::Claude, None, None),
            (Harness::Codex, None, None),
        ];
        for (harness, process, expected) in cases {
            assert_eq!(
                classify(harness, process.as_ref()),
                expected,
                "{harness:?} {process:?}"
            );
        }
    }
}

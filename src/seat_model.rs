//! The model each seat's own session last used, read from the seat's own
//! files — never from the pane label or a quota provider (owner 0930).
//!
//! | Harness | Evidence |
//! |---|---|
//! | Claude Code | the last `assistant` entry whose `message.model` is not `<synthetic>`, in `<CLAUDE_CONFIG_DIR or HOME/.claude>/projects/*/<session>.jsonl` |
//! | Codex | the last `turn_context` model in the session's rollout under `CODEX_HOME` (or `HOME/.codex`) |
//! | Cursor | `model.modelId` in `HOME/.cursor/cli-config.json` |
//!
//! The directories come from the pane's own agent process
//! ([`crate::channel::scan`]). A missing link in that chain is
//! [`SeatModel::Unknown`] (the row shows `?`); a scan that found nothing at
//! all, or a file that could not be read this pass, yields no entry so the
//! pane keeps what it shows — a transient failure must not churn the token.
//!
//! Reads are bounded (newest-first chunks, at most [`MODEL_SCAN_BYTES`]) and
//! remembered per file length and mtime in `seat-models.json`, so an idle
//! seat's transcript is only stat'ed on later passes.

use crate::channel::ProcessChannel;
use crate::herdr::AgentPane;
use crate::model::Harness;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One newest-first read step.
const TAIL_BYTES: u64 = 256 * 1024;
/// How far back from EOF a model is looked for before the seat counts as
/// unknown. Codex writes `turn_context` at turn start, then tool calls and
/// `token_count` lines; a long turn can push the model several megabytes
/// behind EOF (observed live: 1–4 MB). A head read would return the
/// session-start model instead. Chunked reverse reads stay within this budget
/// so a 40 MB file is not scanned on every pass.
pub(crate) const MODEL_SCAN_BYTES: u64 = 8 * 1024 * 1024;
/// Extra bytes kept from the newer chunk so a line that straddles a chunk
/// boundary is complete in the older window (`turn_context` is about 2 KB).
const LINE_OVERLAP_BYTES: u64 = 8 * 1024;
const CACHE_FILE: &str = "seat-models.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatModel {
    /// The full model id the session reported.
    Known(String),
    /// The evidence chain is missing a link; the row shows `?`.
    Unknown,
}

/// Pane id → model for every agent pane in `panes`. A pane without an entry
/// keeps what it shows.
pub fn resolve(
    panes: &[AgentPane],
    scan: Option<&BTreeMap<String, ProcessChannel>>,
    cache_root: Option<&Path>,
) -> BTreeMap<String, SeatModel> {
    let mut resolved = BTreeMap::new();
    // No socket, no /proc, or nothing readable: this pass learned nothing.
    let Some(scan) = scan.filter(|scan| !scan.is_empty()) else {
        return resolved;
    };
    let mut cache = cache_root.map(Cache::load).unwrap_or_default();
    for pane in panes {
        let process = scan.get(&pane.pane_id);
        if let Some(model) = resolve_pane(pane, process, &mut cache) {
            resolved.insert(pane.pane_id.clone(), model);
        }
    }
    if let Some(root) = cache_root {
        cache.store(root);
    }
    resolved
}

fn resolve_pane(
    pane: &AgentPane,
    process: Option<&ProcessChannel>,
    cache: &mut Cache,
) -> Option<SeatModel> {
    let Some(process) = process else {
        return Some(SeatModel::Unknown);
    };
    let paths = &process.paths;
    let session = pane.session.as_ref().and_then(|session| session.id());
    let home = paths.home.as_deref().map(Path::new);
    let found = match pane.harness {
        Harness::Claude => {
            let Some(session) = session else {
                return Some(SeatModel::Unknown);
            };
            let Some(dir) = paths
                .claude_config_dir
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| home.map(|home| home.join(".claude")))
            else {
                return Some(SeatModel::Unknown);
            };
            let key = format!("claude\0{}\0{session}", dir.display());
            let Some(path) = cache.path_for(&key, || claude_transcript(&dir, session)) else {
                return Some(SeatModel::Unknown);
            };
            cache.model_in(&path, claude_model)
        }
        Harness::Codex => {
            let Some(session) = session.or(paths.resume_session.as_deref()) else {
                return Some(SeatModel::Unknown);
            };
            let Some(codex_home) = paths
                .codex_home
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| home.map(|home| home.join(".codex")))
            else {
                return Some(SeatModel::Unknown);
            };
            let key = format!("codex\0{}\0{session}", codex_home.display());
            let Some(path) = cache.path_for(&key, || {
                let ids = [session.to_string()];
                let found = crate::providers::codex::find_rollout_paths(&codex_home, &ids);
                Located::from_candidates(found.into_values())
            }) else {
                return Some(SeatModel::Unknown);
            };
            cache.model_in(&path, crate::providers::codex::parse_rollout_model)
        }
        Harness::Cursor => {
            let Some(home) = home else {
                return Some(SeatModel::Unknown);
            };
            cursor_model(&home.join(".cursor").join("cli-config.json"))
        }
    };
    match found {
        Ok(Some(model)) => Some(SeatModel::Known(model)),
        Ok(None) => Some(SeatModel::Unknown),
        Err(()) => None,
    }
}

/// Where a seat's session file is, as far as this pass can tell.
enum Located {
    One(PathBuf),
    /// No such file, or two different files for one session.
    Missing,
}

impl Located {
    fn from_candidates(candidates: impl IntoIterator<Item = PathBuf>) -> Self {
        let distinct = candidates
            .into_iter()
            .map(|path| fs::canonicalize(&path).unwrap_or(path))
            .collect::<BTreeSet<_>>();
        match distinct.len() {
            1 => Located::One(distinct.into_iter().next().unwrap_or_default()),
            _ => Located::Missing,
        }
    }
}

/// `<dir>/projects/*/<session>.jsonl`. The project directory is named after
/// the launch cwd, which the pane's current cwd need not match, so every
/// project directory is checked.
fn claude_transcript(dir: &Path, session: &str) -> Located {
    let file = format!("{session}.jsonl");
    let Ok(entries) = fs::read_dir(dir.join("projects")) else {
        return Located::Missing;
    };
    Located::from_candidates(
        entries
            .flatten()
            .map(|entry| entry.path().join(&file))
            .filter(|path| path.is_file()),
    )
}

/// The newest real assistant model in a chunk of transcript lines.
/// `<synthetic>` marks a message Claude Code wrote itself, not a model reply.
fn claude_model(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        if !line.contains("\"assistant\"") {
            return None;
        }
        let entry = serde_json::from_str::<Value>(line).ok()?;
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        entry
            .get("message")?
            .get("model")?
            .as_str()
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != "<synthetic>")
            .map(str::to_string)
    })
}

fn cursor_model(path: &Path) -> Result<Option<String>, ()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    let model = config.get("model");
    Ok(["modelId", "displayModelId"].iter().find_map(|key| {
        model?
            .get(key)?
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }))
}

/// Walk `path` newest-first in [`TAIL_BYTES`] chunks until `parse` finds
/// something, stopping [`MODEL_SCAN_BYTES`] from the end. `parse` sees whole
/// lines only. `Err` means the file could not be read.
pub(crate) fn newest_match<T>(
    path: &Path,
    parse: impl Fn(&str) -> Option<T>,
) -> std::io::Result<Option<T>> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(None);
    }
    let floor = length.saturating_sub(MODEL_SCAN_BYTES);
    let mut cursor = length;
    while cursor > floor {
        let start = cursor.saturating_sub(TAIL_BYTES).max(floor);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(cursor.saturating_sub(start))
            .read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut slice = text.as_ref();
        if start > 0 {
            slice = match slice.split_once('\n') {
                Some((_, rest)) => rest,
                None => {
                    let Some(next) = next_cursor(start, cursor, floor) else {
                        break;
                    };
                    cursor = next;
                    continue;
                }
            };
        }
        if cursor < length && !slice.is_empty() && !slice.ends_with('\n') {
            slice = slice.rsplit_once('\n').map(|(rest, _)| rest).unwrap_or("");
        }
        if let Some(found) = parse(slice) {
            return Ok(Some(found));
        }
        let Some(next) = next_cursor(start, cursor, floor) else {
            break;
        };
        cursor = next;
    }
    Ok(None)
}

fn next_cursor(start: u64, cursor: u64, floor: u64) -> Option<u64> {
    if start <= floor {
        return None;
    }
    let next = start.saturating_add(LINE_OVERLAP_BYTES);
    (next < cursor).then_some(next)
}

/// Located paths and read results, remembered across passes.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    sessions: BTreeMap<String, PathBuf>,
    #[serde(default)]
    files: BTreeMap<PathBuf, FileRead>,
    #[serde(skip)]
    changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileRead {
    length: u64,
    modified_nanos: u128,
    model: Option<String>,
}

impl Cache {
    fn load(root: &Path) -> Self {
        fs::read(root.join(CACHE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store(&mut self, root: &Path) {
        if !self.changed {
            return;
        }
        // Forget files that are gone, so the cache tracks live seats only.
        self.files.retain(|path, _| path.is_file());
        self.sessions.retain(|_, path| path.is_file());
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        let temporary = root.join(format!("{CACHE_FILE}.{}.tmp", std::process::id()));
        if fs::write(&temporary, bytes).is_ok()
            && fs::rename(&temporary, root.join(CACHE_FILE)).is_err()
        {
            let _ = fs::remove_file(&temporary);
        }
    }

    /// The remembered path while it still exists, else a fresh lookup.
    /// `None` means the chain is broken (no file, or ambiguous).
    fn path_for(&mut self, key: &str, locate: impl FnOnce() -> Located) -> Option<PathBuf> {
        if let Some(path) = self.sessions.get(key).filter(|path| path.is_file()) {
            return Some(path.clone());
        }
        match locate() {
            Located::One(path) => {
                self.sessions.insert(key.to_string(), path.clone());
                self.changed = true;
                Some(path)
            }
            Located::Missing => {
                if self.sessions.remove(key).is_some() {
                    self.changed = true;
                }
                None
            }
        }
    }

    /// The newest model in `path`, re-read only when the file changed.
    fn model_in(
        &mut self,
        path: &Path,
        parse: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, ()> {
        let metadata = fs::metadata(path).map_err(|_| ())?;
        let length = metadata.len();
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        if let Some(read) = self
            .files
            .get(path)
            .filter(|read| read.length == length && read.modified_nanos == modified_nanos)
        {
            return Ok(read.model.clone());
        }
        let model = newest_match(path, parse).map_err(|_| ())?;
        self.files.insert(
            path.to_path_buf(),
            FileRead {
                length,
                modified_nanos,
                model: model.clone(),
            },
        );
        self.changed = true;
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::SeatPaths;
    use crate::herdr::{AgentSession, AgentStatus};

    fn pane(id: &str, harness: Harness, session: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: id.to_string(),
            workspace_id: "w1".to_string(),
            harness,
            session: session.map(|value| AgentSession {
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

    fn process(paths: SeatPaths) -> ProcessChannel {
        ProcessChannel {
            paths,
            ..ProcessChannel::default()
        }
    }

    fn assistant(model: &str) -> String {
        format!(r#"{{"type":"assistant","message":{{"model":"{model}","content":[]}}}}"#)
    }

    #[test]
    fn the_last_real_assistant_model_wins_and_synthetic_is_skipped() {
        let text = [
            assistant("claude-opus-5"),
            r#"{"type":"user","message":{"content":"x"}}"#.to_string(),
            assistant("claude-opus-5-5"),
            assistant("<synthetic>"),
            r#"{"type":"system","subtype":"compact"}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(claude_model(&text).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(claude_model(&assistant("<synthetic>")), None);
        assert_eq!(claude_model(r#"{"type":"user"}"#), None);
    }

    #[test]
    fn a_model_far_behind_the_tail_is_still_found_within_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("t.jsonl");
        // 1 MiB of user lines after the last reply: four chunks back.
        let filler = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "a".repeat(1000)
        );
        let mut text = assistant("deepseek-v4.1-flash-dspark-greedy-1m-h200");
        for _ in 0..1024 {
            text.push('\n');
            text.push_str(&filler);
        }
        text.push('\n');
        fs::write(&path, text).unwrap();
        assert_eq!(
            newest_match(&path, claude_model).unwrap().as_deref(),
            Some("deepseek-v4.1-flash-dspark-greedy-1m-h200")
        );
        assert!(newest_match(&directory.path().join("missing"), claude_model).is_err());
    }

    #[test]
    fn cursor_reads_the_model_id_from_its_cli_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli-config.json");
        assert_eq!(cursor_model(&path), Ok(None));
        fs::write(
            &path,
            r#"{"model":{"modelId":"grok-4.7","displayName":"Grok 4.7 256K High Fast"}}"#,
        )
        .unwrap();
        assert_eq!(cursor_model(&path), Ok(Some("grok-4.7".to_string())));
        fs::write(&path, r#"{"model":{"displayModelId":"gpt-6-sol"}}"#).unwrap();
        assert_eq!(cursor_model(&path), Ok(Some("gpt-6-sol".to_string())));
        fs::write(&path, "not json").unwrap();
        assert_eq!(cursor_model(&path), Ok(None));
    }

    #[test]
    fn each_harness_resolves_from_its_own_files_and_a_broken_chain_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("claude-config");
        let project = config.join("projects").join("-home-alice-repo");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("s-claude.jsonl"),
            [assistant("claude-sonnet-5-5"), assistant("<synthetic>")].join("\n"),
        )
        .unwrap();
        let codex_home = root.path().join("codex");
        let day = codex_home.join("sessions/2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-09-30T01-00-00-01a0cce2-646d.jsonl"),
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#,
        )
        .unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/cli-config.json"),
            r#"{"model":{"modelId":"grok-4.7"}}"#,
        )
        .unwrap();
        let state = root.path().join("state");
        fs::create_dir_all(&state).unwrap();

        let panes = [
            pane("w1:p1", Harness::Claude, Some("s-claude")),
            // Codex without a Herdr session: the argv `resume <id>` names it.
            pane("w1:p2", Harness::Codex, None),
            pane("w1:p3", Harness::Cursor, None),
            // A Claude pane whose transcript does not exist.
            pane("w1:p4", Harness::Claude, Some("s-gone")),
            // No agent process found for this pane.
            pane("w1:p5", Harness::Codex, Some("x")),
            pane("w1:p6", Harness::Claude, None),
        ];
        let scan = BTreeMap::from([
            (
                "w1:p1".to_string(),
                process(SeatPaths {
                    home: Some("/nonexistent".to_string()),
                    claude_config_dir: Some(config.display().to_string()),
                    ..SeatPaths::default()
                }),
            ),
            (
                "w1:p2".to_string(),
                process(SeatPaths {
                    codex_home: Some(codex_home.display().to_string()),
                    resume_session: Some("01a0cce2-646d".to_string()),
                    ..SeatPaths::default()
                }),
            ),
            (
                "w1:p3".to_string(),
                process(SeatPaths {
                    home: Some(home.display().to_string()),
                    ..SeatPaths::default()
                }),
            ),
            (
                "w1:p4".to_string(),
                process(SeatPaths {
                    claude_config_dir: Some(config.display().to_string()),
                    ..SeatPaths::default()
                }),
            ),
            (
                "w1:p6".to_string(),
                process(SeatPaths {
                    claude_config_dir: Some(config.display().to_string()),
                    ..SeatPaths::default()
                }),
            ),
        ]);
        let known = |model: &str| SeatModel::Known(model.to_string());
        let resolved = resolve(&panes, Some(&scan), Some(&state));
        assert_eq!(resolved["w1:p1"], known("claude-sonnet-5-5"));
        assert_eq!(resolved["w1:p2"], known("gpt-6.1-sol"));
        assert_eq!(resolved["w1:p3"], known("grok-4.7"));
        assert_eq!(resolved["w1:p4"], SeatModel::Unknown);
        assert_eq!(resolved["w1:p5"], SeatModel::Unknown);
        assert_eq!(resolved["w1:p6"], SeatModel::Unknown);
        // The located paths and reads are remembered for the next pass.
        let cache = Cache::load(&state);
        assert_eq!(cache.sessions.len(), 2);
        assert_eq!(cache.files.len(), 2);
        assert_eq!(resolve(&panes, Some(&scan), Some(&state)), resolved);

        // A scan that found no process at all learned nothing: every pane
        // keeps what it shows.
        assert!(resolve(&panes, Some(&BTreeMap::new()), Some(&state)).is_empty());
        assert!(resolve(&panes, None, Some(&state)).is_empty());
    }

    #[test]
    fn two_different_transcripts_for_one_session_are_unknown() {
        let root = tempfile::tempdir().unwrap();
        for project in ["-a", "-b"] {
            let directory = root.path().join("projects").join(project);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("s.jsonl"), assistant("claude-opus-5-5")).unwrap();
        }
        assert!(matches!(
            claude_transcript(root.path(), "s"),
            Located::Missing
        ));
        fs::remove_file(root.path().join("projects/-b/s.jsonl")).unwrap();
        assert!(matches!(
            claude_transcript(root.path(), "s"),
            Located::One(_)
        ));
    }
}

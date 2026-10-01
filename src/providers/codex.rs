use crate::cache::CacheStore;
use crate::model::{
    CacheTotals, CacheUsage, ContextUsage, Provider, ProviderSnapshot, ResetAt, UsageWindow,
    WindowKind,
};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const FIVE_HOUR_WINDOW_MINUTES: u64 = 5 * 60;
const WEEKLY_WINDOW_MINUTES: u64 = 7 * 24 * 60;
const ROLLOUT_TAIL_BYTES: u64 = 256 * 1024;
const CODEX_CONTEXT_BASELINE_TOKENS: u64 = 12_000;
/// Prompt cache lifetime assumed for a Codex request.
///
/// Codex never records a TTL or an expiry: the rollout JSONL only carries
/// `cached_input_tokens` / `cache_write_input_tokens` and the request
/// timestamp, and the `prompt_cache_options` the Responses API returns is
/// dropped by the Codex SSE parser before it reaches disk. OpenAI documents
/// `30m` as the default and currently only supported `prompt_cache_options.ttl`,
/// so the sidebar anchors that TTL to the last recorded request. This is an
/// estimate, not a server-reported expiry — the sidebar labels it `ttl≈`.
pub(crate) const CODEX_PROMPT_CACHE_TTL_SECONDS: u64 = 30 * 60;

pub fn parse_rate_limits(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let result = value.get("result").unwrap_or(value);
    let windows = collect_codex_windows(result);
    if windows.is_empty() {
        return Err(ProviderError::UnsupportedResponse(
            "no supported rate limit windows".to_string(),
        ));
    }
    Ok(ProviderSnapshot::new(
        Provider::Codex,
        windows,
        fetched_at_unix,
    ))
}

fn collect_codex_windows(value: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    let mut push_from = |limits: &Value| {
        for candidate in [limits.get("primary"), limits.get("secondary")]
            .into_iter()
            .flatten()
        {
            let Some(window) = parse_codex_window(candidate) else {
                continue;
            };
            if windows
                .iter()
                .any(|existing: &UsageWindow| existing.kind == window.kind)
            {
                continue;
            }
            windows.push(window);
        }
    };
    if let Some(limits) = value.get("rateLimits").or_else(|| value.get("rate_limits")) {
        push_from(limits);
    }
    if let Some(by_id) = value
        .get("rateLimitsByLimitId")
        .or_else(|| value.get("rate_limits_by_limit_id"))
        .and_then(Value::as_object)
    {
        for limits in by_id.values() {
            push_from(limits);
        }
    }
    if value.get("primary").is_some() || value.get("secondary").is_some() {
        push_from(value);
    }
    windows
}

fn parse_codex_window(candidate: &Value) -> Option<UsageWindow> {
    if candidate.is_null() {
        return None;
    }
    let kind = candidate
        .get("windowDurationMins")
        .or_else(|| candidate.get("window_duration_mins"))
        .or_else(|| candidate.get("window_minutes"))
        .and_then(json_u64)
        .and_then(window_kind)?;
    let used = candidate
        .get("usedPercent")
        .or_else(|| candidate.get("used_percent"))
        .and_then(Value::as_f64)?;
    let reset = candidate
        .get("resetsAt")
        .or_else(|| candidate.get("resets_at"))
        .and_then(parse_reset);
    UsageWindow::new(kind, used, reset).ok()
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let number = value.as_f64()?;
        (number.is_finite() && number >= 0.0).then_some(number.round() as u64)
    })
}

fn window_kind(duration_minutes: u64) -> Option<WindowKind> {
    // Token-count headers sometimes report 299 / 10079 remaining minutes
    // instead of the nominal 300 / 10080 window length.
    if duration_minutes.abs_diff(FIVE_HOUR_WINDOW_MINUTES) <= 60 {
        Some(WindowKind::FiveHour)
    } else if duration_minutes.abs_diff(WEEKLY_WINDOW_MINUTES) <= 180 {
        Some(WindowKind::Weekly)
    } else {
        None
    }
}

fn parse_reset(value: &Value) -> Option<ResetAt> {
    json_u64(value)
        .map(ResetAt::from_unix_seconds)
        .or_else(|| value.as_str().and_then(ResetAt::parse))
}

/// Fetch quota and supplement it with local diagnostics for the sessions
/// currently visible in Herdr. The empty slice is used by direct CLI calls;
/// the refresh path supplies pane session ids so an older pane is not lost
/// behind the bounded `thread/list` page.
pub fn fetch_for_sessions(session_ids: &[String]) -> Result<ProviderSnapshot> {
    let executable = std::env::var_os("CODEX_BIN_PATH").unwrap_or_else(|| "codex".into());
    let mut command = Command::new(executable);
    command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (mut child, tree) =
        crate::process::spawn_constrained(&mut command).context("start codex app-server")?;
    let mut input = child.stdin.take().context("open codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("open codex app-server stdout")?;
    let mut output = BufReader::new(stdout);

    request_quota(
        &mut input,
        &mut output,
        child,
        tree,
        REQUEST_TIMEOUT,
        session_ids,
    )
}

/// Ask the app-server, ending its tree if the request runs past `timeout`.
///
/// `timeout` is a parameter so the entry point can be exercised without
/// waiting the real fifteen seconds, and so the timeout branch — where the
/// watchdog, not this thread, is the one that ends the tree — can be run
/// (Codex g1e).
fn request_quota(
    input: &mut ChildStdin,
    output: &mut BufReader<impl std::io::Read>,
    child: Child,
    tree: crate::process::Tree,
    timeout: Duration,
    session_ids: &[String],
) -> Result<ProviderSnapshot> {
    // The watchdog and this thread share the child, so it can only ever be
    // signalled while it is still unreaped. Signalling a bare pid after
    // `wait` would race with the operating system recycling that pid.
    let child = Arc::new(Mutex::new(Some((child, tree))));
    // Whoever ends the tree reports its own failure here; the other thread
    // finds the child already taken. An empty slot is not proof that the
    // tree ended, so the failure has to be kept somewhere both threads can
    // reach (Codex g1e).
    let end_failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (cancel, cancelled) = mpsc::channel();
    let watchdog = Arc::clone(&child);
    let watchdog_end_failure = Arc::clone(&end_failure);
    let watchdog = thread::spawn(move || {
        // Sixty seconds of sleeping would be joined below, so the main
        // thread cancels this once it has taken the child itself: the join
        // then waits only for a watchdog that has actually timed out
        // (Codex g1f).
        if cancelled.recv_timeout(timeout).is_err() {
            if let Err(error) = terminate(&watchdog) {
                let mut slot = watchdog_end_failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                *slot = Some(format!(
                    "the timed-out codex app-server could not be ended: {error:#}"
                ));
            }
        }
    });

    let fetched = fetch_from_process(input, output, session_ids);
    let ended = terminate(&child);
    let _ = cancel.send(());
    // Read the slot only after the watchdog is done: otherwise a failure it
    // is still recording could be missed (Codex g1f).
    let _ = watchdog.join();
    let timed_out_without_ending = end_failure
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    let end_failure = match (ended, timed_out_without_ending) {
        (Err(error), _) => Some(format!("{error:#}")),
        (Ok(()), failure) => failure,
    };
    match (fetched, end_failure) {
        (Ok(snapshot), None) => Ok(snapshot),
        // The fetch's own error stays the one the caller recovers from; the
        // termination failure rides along in its chain rather than replacing
        // it.
        (Err(error), failure) => Err(match failure {
            Some(failure) => error.context(failure),
            None => error,
        }),
        (Ok(_), Some(failure)) => Err(anyhow::anyhow!(failure)),
    }
}

/// Kill the app-server's process group and reap it, at most once.
///
/// Whichever of the request thread and the watchdog gets here first takes the
/// child; the other one finds an empty slot and does nothing.
fn terminate(child: &Mutex<Option<(Child, crate::process::Tree)>>) -> Result<()> {
    let Ok(mut slot) = child.lock() else {
        anyhow::bail!("lock codex app-server child");
    };
    let Some((mut child, tree)) = slot.take() else {
        return Ok(());
    };
    // `pre_exec` put the app-server in its own process group (a job object
    // on Windows), so this also collects any helper it spawned.
    tree.end(&mut child)?;
    let _ = child.wait();
    Ok(())
}

fn fetch_from_process(
    input: &mut ChildStdin,
    output: &mut BufReader<impl std::io::Read>,
    requested_session_ids: &[String],
) -> Result<ProviderSnapshot> {
    write_rpc(
        input,
        1,
        "initialize",
        serde_json::json!({
            "clientInfo": {"name": "herdr-agent-quota", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {}
        }),
    )?;
    let _ = read_rpc(output, 1)?;
    write_notification(input, "initialized", serde_json::json!({}))?;

    write_rpc(input, 2, "account/read", serde_json::json!({}))?;
    let account = read_rpc(output, 2)?;
    if !account_is_chatgpt(&account) {
        anyhow::bail!(ProviderError::Unavailable(
            "Codex is using API-key auth, not a ChatGPT subscription".to_string()
        ));
    }

    write_rpc(input, 3, "account/rateLimits/read", serde_json::json!({}))?;
    let limits = read_rpc(output, 3)?;
    let mut snapshot =
        parse_rate_limits(&limits, CacheStore::now_unix()).map_err(anyhow::Error::from)?;
    snapshot.account_id = current_account_id().or_else(|| account_id_from_rpc(&account));

    // Session previews come from Codex's local state database. This is one
    // bounded read in the same app-server process as the quota request; it
    // does not resume threads, scan rollout JSONL, or contact the model.
    write_rpc(
        input,
        4,
        "thread/list",
        serde_json::json!({
            "limit": 50,
            "sortKey": "updated_at",
            "useStateDbOnly": true
        }),
    )?;
    let mut session_ids = requested_session_ids.to_vec();
    if let Ok(threads) = read_rpc(output, 4) {
        snapshot.session_summaries = parse_session_summaries(&threads);
        for session_id in parse_thread_ids(&threads) {
            if !session_ids.contains(&session_id) {
                session_ids.push(session_id);
            }
        }
    }
    enrich_local_sessions(&mut snapshot, &session_ids);
    Ok(snapshot)
}

fn parse_thread_ids(value: &Value) -> Vec<String> {
    let result = value.get("result").unwrap_or(value);
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thread| {
            thread
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// Supplement quota data with bounded, local-only reads from the rollout
/// belonging to each thread returned by `thread/list`. The app-server request
/// above does not expose live token usage, while the rollout tail does. We
/// never resume a thread, read prompt text into memory, or scan every pane's
/// output; only the matching JSONL filenames are opened.
fn enrich_local_sessions(snapshot: &mut ProviderSnapshot, session_ids: &[String]) {
    let Some(home) = codex_home().ok() else {
        return;
    };
    enrich_local_sessions_at(snapshot, &home, session_ids);
}

fn enrich_local_sessions_at(snapshot: &mut ProviderSnapshot, home: &Path, session_ids: &[String]) {
    if session_ids.is_empty() {
        return;
    }
    let mut newest: Option<(u64, Option<String>, ContextUsage)> = None;
    let rollout_paths = find_rollout_paths(home, session_ids);
    for session_id in session_ids {
        let Some(path) = rollout_paths.get(session_id) else {
            continue;
        };
        let Some(observation) = read_rollout_observation(path, session_id) else {
            continue;
        };
        if let Some(model) = observation.model.clone() {
            snapshot.session_models.insert(session_id.clone(), model);
        }
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        let Some(context) = observation.context else {
            continue;
        };
        snapshot
            .session_contexts
            .insert(session_id.clone(), context.clone());

        if newest
            .as_ref()
            .is_none_or(|(current, _, _)| modified >= *current)
        {
            newest = Some((modified, observation.model, context));
        }
    }
    if let Some((_, model, context)) = newest {
        if model.is_some() {
            snapshot.model = model;
        }
        snapshot.context = Some(context);
    }
}

pub(crate) fn find_rollout_paths(home: &Path, session_ids: &[String]) -> BTreeMap<String, PathBuf> {
    walk_rollouts(home, session_ids, false).unwrap_or_default()
}

/// [`find_rollout_paths`], but a directory or entry that cannot be read is an
/// error instead of being skipped, so a caller can tell "no rollout" from
/// "could not look". A missing `sessions` / `archived_sessions` is not an
/// error.
pub(crate) fn find_rollout_paths_strict(
    home: &Path,
    session_ids: &[String],
) -> std::io::Result<BTreeMap<String, PathBuf>> {
    walk_rollouts(home, session_ids, true)
}

fn walk_rollouts(
    home: &Path,
    session_ids: &[String],
    strict: bool,
) -> std::io::Result<BTreeMap<String, PathBuf>> {
    let mut directories = vec![home.join("sessions"), home.join("archived_sessions")];
    let mut newest = BTreeMap::<String, (u64, PathBuf)>::new();
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if strict && error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error);
            }
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if strict => return Err(error),
                Err(_) => continue,
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) if strict => return Err(error),
                Err(_) => continue,
            };
            if file_type.is_dir() {
                directories.push(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
            {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let matching_ids = session_ids
                .iter()
                .filter(|session_id| name.contains(session_id.as_str()));
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            for session_id in matching_ids {
                let replace = newest
                    .get(session_id)
                    .is_none_or(|(current, _)| modified >= *current);
                if replace {
                    newest.insert(session_id.clone(), (modified, path.clone()));
                }
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(session_id, (_, path))| (session_id, path))
        .collect())
}

struct RolloutObservation {
    model: Option<String>,
    context: Option<ContextUsage>,
}

fn read_rollout_observation(path: &Path, session_id: &str) -> Option<RolloutObservation> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(ROLLOUT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start == 0 {
        text.into_owned()
    } else {
        text.split_once('\n')?.1.to_string()
    };
    let mut observation = parse_rollout_observation(&text, session_id)?;
    if observation.model.is_none() {
        // The tail is live token_count / tool output. The latest model sits
        // further back, at the start of this turn. Never fall back to the
        // file head: that is the first turn's model, not the current one.
        observation.model = read_latest_rollout_model(path);
    }
    Some(observation)
}

/// The latest `turn_context` model, newest-first (see
/// [`crate::seat_model::newest_match`]). Never the file head: that is the
/// first turn's model, not the current one.
fn read_latest_rollout_model(path: &Path) -> Option<String> {
    crate::seat_model::newest_match(path, parse_rollout_model)
        .ok()
        .flatten()
}

fn parse_rollout_observation(text: &str, session_id: &str) -> Option<RolloutObservation> {
    let mut model = None;
    let mut context = None;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) == Some("turn_context") {
            let payload = entry.get("payload").unwrap_or(&entry);
            model = parse_model_payload(payload).or(model);
            continue;
        }
        if entry.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let payload = entry.get("payload").unwrap_or(&entry);
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        // Rollouts do not identify the serving account. A still-running old
        // session can write after auth.json changes, so its quota must never
        // supplement the current account's app-server response.
        let info = payload.get("info").unwrap_or(payload);
        let Some(last) = info
            .get("last_token_usage")
            .or_else(|| info.get("lastTokenUsage"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(window) = info
            .get("model_context_window")
            .or_else(|| info.get("modelContextWindow"))
            .and_then(Value::as_u64)
        else {
            continue;
        };
        let total = token_count(last, "total_tokens", "totalTokens");
        if total == 0 || window <= CODEX_CONTEXT_BASELINE_TOKENS {
            continue;
        }
        let used = total.saturating_sub(CODEX_CONTEXT_BASELINE_TOKENS) as f64
            / (window - CODEX_CONTEXT_BASELINE_TOKENS) as f64
            * 100.0;
        let Some(info_object) = info.as_object() else {
            continue;
        };
        let cache = parse_rollout_cache(info_object).map(|cache| {
            let totals = CacheTotals::from_token_counts(
                cache.fresh_input_tokens,
                cache.read_tokens,
                cache.creation_tokens,
            );
            let cache = cache.with_session_totals(totals, session_id, 0);
            // Every request refreshes the prefix cache, so the entry's own
            // timestamp is the anchor. `cache_write_input_tokens` stays 0 on
            // ChatGPT-backed sessions even while reads are large, so it cannot
            // be used to pick the anchor.
            match parse_rollout_timestamp(&entry) {
                Some(requested_at) => {
                    cache.with_ttl_estimate(CODEX_PROMPT_CACHE_TTL_SECONDS, requested_at)
                }
                None => cache,
            }
        });
        let context_value = ContextUsage::new(used.clamp(0.0, 100.0))
            .ok()?
            .with_cache(cache);
        context = Some(context_value);
    }
    Some(RolloutObservation { model, context })
}

fn parse_rollout_timestamp(entry: &Value) -> Option<u64> {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse_rfc3339)
        .map(ResetAt::unix_seconds)
}

pub(crate) fn parse_rollout_model(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let entry = serde_json::from_str::<Value>(line).ok()?;
        (entry.get("type").and_then(Value::as_str) == Some("turn_context"))
            .then(|| parse_model_payload(entry.get("payload").unwrap_or(&entry)))
            .flatten()
    })
}

fn parse_model_payload(payload: &Value) -> Option<String> {
    payload
        .get("model")
        .or_else(|| payload.get("model_slug"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

fn parse_rollout_cache(info: &serde_json::Map<String, Value>) -> Option<CacheUsage> {
    let totals = info
        .get("total_token_usage")
        .or_else(|| info.get("totalTokenUsage"))
        .and_then(Value::as_object)
        .or_else(|| {
            info.get("last_token_usage")
                .or_else(|| info.get("lastTokenUsage"))
                .and_then(Value::as_object)
        })?;
    let input = token_count(totals, "input_tokens", "inputTokens");
    let read = token_count(totals, "cached_input_tokens", "cachedInputTokens");
    let creation =
        token_count(totals, "cache_write_input_tokens", "cacheWriteInputTokens").max(token_count(
            totals,
            "cache_creation_input_tokens",
            "cacheCreationInputTokens",
        ));
    CacheUsage::from_token_counts(input.saturating_sub(read), read, creation)
}

fn token_count(object: &serde_json::Map<String, Value>, snake: &str, camel: &str) -> u64 {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn parse_session_summaries(value: &Value) -> BTreeMap<String, String> {
    let result = value.get("result").unwrap_or(value);
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thread| {
            let id = thread.get("id").and_then(Value::as_str)?;
            let preview = thread.get("preview").and_then(Value::as_str)?;
            let summary = preview
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .filter(|line| !line.eq_ignore_ascii_case("ask codex to do anything"))
                .map(truncate_summary)?;
            Some((id.to_string(), summary))
        })
        .collect()
}

fn truncate_summary(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= 80 {
        return value.to_string();
    }
    let mut summary: String = characters.into_iter().take(77).collect();
    summary.push('…');
    summary
}

fn write_rpc(input: &mut ChildStdin, id: u64, method: &str, params: Value) -> Result<()> {
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(input, "{}", serde_json::to_string(&message)?)?;
    input.flush()?;
    Ok(())
}

fn write_notification(input: &mut ChildStdin, method: &str, params: Value) -> Result<()> {
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    });
    writeln!(input, "{}", serde_json::to_string(&message)?)?;
    input.flush()?;
    Ok(())
}

fn read_rpc(output: &mut BufReader<impl std::io::Read>, expected_id: u64) -> Result<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let count = output.read_line(&mut line)?;
        if count == 0 {
            anyhow::bail!("Codex app-server exited before response {expected_id}");
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) != Some(expected_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server request failed: {}", safe_rpc_error(error));
        }
        return Ok(value);
    }
}

/// What is safe to quote from an app-server error.
///
/// `error` is upstream's object. `data` may carry anything the service chose
/// — including a credential — and `message` is free text, so only a named
/// code and a message stripped of credential-shaped runs is kept, and this
/// string is what reaches the recorded diagnostic file (Codex g1c G2).
/// What is safe to quote from an app-server error: the stage, and the
/// integer code JSON-RPC defines.
///
/// Nothing of upstream's own text. `data` may carry a credential, and
/// `message` is free text whose credential shapes cannot be enumerated —
/// three rounds of review each found more of them (a password may hold the
/// sub-delimiters a URL allows, a quoted value may hold spaces, an
/// authentication scheme is matched without regard to case). Rather than
/// keep guessing at delimiters, the persistent diagnostic for an RPC failure
/// is a fixed stage plus a validated integer code, and the message is not
/// quoted at all (Codex g1i). The startup failure that matters most on
/// Windows — the OS error under `start codex app-server` — does not come
/// through here and keeps its whole chain.
fn safe_rpc_error(error: &Value) -> String {
    match error.get("code") {
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            format!("code {number}")
        }
        _ => "an error with no integer code".to_string(),
    }
}

pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(codex_home()?.join("auth.json"))
}

fn codex_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    Ok(std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex")))
}

pub fn current_account_id() -> Option<String> {
    account_id_from_auth(&auth_path().ok()?)
}

pub fn auth_mtime_unix() -> Option<u64> {
    CacheStore::file_mtime_unix(&auth_path().ok()?)
}

pub fn account_id_from_auth(path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct AuthMetadata {
        tokens: Option<TokenMetadata>,
    }

    #[derive(Deserialize)]
    struct TokenMetadata {
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default, alias = "chatgptAccountId")]
        chatgpt_account_id: Option<String>,
    }

    // Only materialize the stable account id. Token fields are ignored by the
    // streaming deserializer and never enter an owned Rust value.
    let metadata: AuthMetadata =
        serde_json::from_reader(BufReader::new(fs::File::open(path).ok()?)).ok()?;
    let tokens = metadata.tokens?;
    tokens
        .account_id
        .or(tokens.chatgpt_account_id)
        .filter(|value| !value.is_empty())
}

fn account_id_from_rpc(value: &Value) -> Option<String> {
    let result = value.get("result").unwrap_or(value);
    let account = result.get("account").unwrap_or(result);
    ["accountId", "account_id", "chatgptAccountId", "id"]
        .iter()
        .find_map(|key| account.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty() && *value != "chatgpt")
        .map(str::to_string)
}

pub fn account_is_chatgpt(value: &Value) -> bool {
    let result = value.get("result").unwrap_or(value);
    let account = result.get("account").unwrap_or(result);
    let auth_mode = account
        .get("authMode")
        .or_else(|| account.get("auth_mode"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let account_type = account
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan = account
        .get("plan")
        .and_then(Value::as_str)
        .unwrap_or_default();
    [auth_mode, account_type, plan]
        .iter()
        .any(|value| value.to_ascii_lowercase().contains("chatgpt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A child running `command`, contained the way a real app-server is:
    /// its own process group on Unix (`setpgid`, as both production callers
    /// do) so the group kill is really exercised, and a job object on
    /// Windows (Codex g1f).
    fn contained_server(
        command: &str,
    ) -> (
        ChildStdin,
        BufReader<std::process::ChildStdout>,
        Child,
        crate::process::Tree,
    ) {
        let mut server = Command::new("sh");
        server
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        unsafe {
            server.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let (mut child, tree) = crate::process::spawn_constrained(&mut server).unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        (input, output, child, tree)
    }

    #[test]
    fn a_timeout_whose_tree_cannot_be_ended_is_reported_to_the_caller() {
        // Codex g1e: the watchdog, not this thread, is the one that ends a
        // timed-out app-server's tree, so its failure has to be kept where
        // the caller can find it. No network, no credentials: the child is
        // `sh -c 'sleep 1'`, which outlives the 50 ms timeout and answers
        // nothing, so the request thread is still blocked when the watchdog
        // takes the child.
        let (mut input, mut output, child, real_tree) = contained_server("sleep 0.5");
        let error = request_quota(
            &mut input,
            &mut output,
            child,
            crate::process::testing::failing_tree(),
            Duration::from_millis(50),
            &[],
        )
        .unwrap_err();
        drop(real_tree);
        let chain = format!("{error:#}");
        assert!(
            chain.contains("the timed-out codex app-server could not be ended"),
            "only the watchdog writes this line: {chain}"
        );
        assert!(
            chain.contains("TerminateJobObject failed (win32 error 1234)"),
            "the stage and the code must reach the caller: {chain}"
        );
    }

    #[test]
    fn a_timeout_whose_tree_ends_cleanly_reports_no_termination_failure() {
        // The positive side of the same entry: the watchdog ends the tree
        // for real, and the caller sees only the fetch's own error.
        let (mut input, mut output, child, tree) = contained_server("sleep 0.5");
        let error = request_quota(
            &mut input,
            &mut output,
            child,
            tree,
            Duration::from_millis(50),
            &[],
        )
        .unwrap_err();
        let chain = format!("{error:#}");
        assert!(!chain.contains("could not be ended"), "{chain}");
    }

    #[test]
    fn a_fake_app_server_yields_the_subscription_windows() {
        // The happy path through the same entry, end to end and offline: the
        // "app-server" is a shell loop that answers each request line with
        // the id it was given.
        let script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) inner='{}' ;;
    *account/read*) inner='{"account":{"type":"chatgpt"}}' ;;
    *rateLimits/read*) inner='{"rateLimits":{"primary":{"usedPercent":12.0,"windowDurationMins":300,"resetsAt":1786795200},"secondary":{"usedPercent":24.0,"windowDurationMins":10080,"resetsAt":1787400000}}}' ;;
    *thread/list*) inner='{"data":[]}' ;;
    *) continue ;;
  esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$inner"
done
"#;
        let (mut input, mut output, child, tree) = contained_server(script);
        // `request_quota` reads the account from the credential root, so the
        // test must not run against the real one: both variables point into
        // a temporary directory for as long as it runs (Codex g1f).
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::providers::test_support::env_guard();
        let previous_auth = std::env::var_os("CODEX_AUTH_FILE");
        let previous_home = std::env::var_os("CODEX_HOME");
        std::env::set_var("CODEX_AUTH_FILE", root.path().join("auth.json"));
        std::env::set_var("CODEX_HOME", root.path());
        let fetched = request_quota(
            &mut input,
            &mut output,
            child,
            tree,
            Duration::from_secs(5),
            &[],
        );
        match previous_auth {
            Some(value) => std::env::set_var("CODEX_AUTH_FILE", value),
            None => std::env::remove_var("CODEX_AUTH_FILE"),
        }
        match previous_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        let snapshot = fetched.unwrap();
        assert!(snapshot.window(WindowKind::FiveHour).is_some());
        assert!(snapshot.window(WindowKind::Weekly).is_some());
        assert!(
            snapshot.account_id.is_none(),
            "the test must not pick up the machine's real account"
        );
    }

    /// Every hostile shape the three review rounds produced, in one error
    /// frame, driven through the real RPC path.
    const HOSTILE_MESSAGE: &str = "denied: GET https://api.example.com/v1 via https://user:pw@proxy.example.com, api_key=%s, Authorization: bearer %s, password=prefix %s end";
    const SENTINEL: &str = "credential-sentinel-9f2c";

    fn hostile_error_server() -> String {
        format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  printf '{{"jsonrpc":"2.0","id":%s,"error":{{"code":-32000,"message":"{HOSTILE_MESSAGE}","data":{{"access_token":"%s","nested":{{"refresh_token":"%s"}}}}}}}}\n' "$id" "{SENTINEL}" "{SENTINEL}" "{SENTINEL}" "{SENTINEL}"
done
"#
        )
    }

    #[test]
    fn an_rpc_error_body_never_reaches_the_caller_verbatim() {
        // Codex g1i: upstream's message is not quoted at all. Three rounds
        // found three more credential shapes each time, so the persistent
        // diagnostic is the stage plus the integer code, and nothing else.
        let (mut input, mut output, child, tree) = contained_server(&hostile_error_server());
        let error = request_quota(
            &mut input,
            &mut output,
            child,
            tree,
            Duration::from_secs(5),
            &[],
        )
        .unwrap_err();
        let chain = format!("{error:#}");
        assert!(
            chain.contains("Codex app-server request failed: code -32000"),
            "the stage and the code must survive: {chain}"
        );
        assert!(
            !chain.contains(SENTINEL),
            "a credential reached the caller: {chain}"
        );
        assert!(
            !chain.contains("denied")
                && !chain.contains("access_token")
                && !chain.contains("refresh_token")
                && !chain.contains("user:pw@")
                && !chain.contains("password"),
            "no part of the message or the payload may be quoted: {chain}"
        );
    }

    #[test]
    fn an_error_message_is_never_quoted_however_it_is_shaped() {
        // The boundary shapes Codex g1i listed, plus the empty and the very
        // long message, all through the same projection.
        let long = "x".repeat(4096);
        for message in [
            "",
            " ",
            "denied",
            "user:prefix&sentinel@api.example.com",
            "password=\"prefix sentinel\"",
            "Authorization: bEaReR sentinel",
            "Authorization: basic sentinel",
            long.as_str(),
            "https://user:prefix;sentinel@api.example.com/v1",
            "https://user:prefix,sentinel@api.example.com/v1",
        ] {
            assert_eq!(
                safe_rpc_error(&json!({"code": -32000, "message": message})),
                "code -32000",
                "{message}"
            );
            assert_eq!(
                safe_rpc_error(&json!({"message": message})),
                "an error with no integer code",
                "{message}"
            );
        }
    }

    #[test]
    fn a_string_code_is_not_quoted_but_an_integer_one_is() {
        // JSON-RPC's code is an integer, and a string code is upstream's free
        // text like the message.
        assert_eq!(
            safe_rpc_error(&json!({"code": SENTINEL, "message": "denied"})),
            "an error with no integer code"
        );
        assert_eq!(
            safe_rpc_error(&json!({"code": -32000, "message": "denied"})),
            "code -32000"
        );
        // An object code is as untrusted as a string one.
        assert_eq!(
            safe_rpc_error(&json!({"code": {"secret": SENTINEL}, "message": "denied"})),
            "an error with no integer code"
        );
        // A float is not JSON-RPC's error code either.
        assert_eq!(
            safe_rpc_error(&json!({"code": -32000.5, "message": "denied"})),
            "an error with no integer code"
        );
    }

    #[test]
    fn redaction_keeps_what_can_be_read_and_hides_what_cannot() {
        // Kept as the positive control for the shapes that are ordinary
        // text: nothing here is quoted from upstream any more, so a message
        // that is only words changes nothing.
        assert_eq!(
            safe_rpc_error(&json!({"code": -32000, "message": "no credentials in this sentence"})),
            "code -32000"
        );
    }

    #[test]
    fn an_error_without_a_nameable_part_does_not_quote_the_payload() {
        // Nothing nameable: the diagnostic is still just the fixed phrase,
        // and the payload is never quoted.
        let error = json!({"data": {"secret": "credential-sentinel-9f2c"}});
        let projected = safe_rpc_error(&error);
        assert_eq!(projected, "an error with no integer code");
        assert!(!projected.contains("credential-sentinel"));
    }

    /// The recorded diagnostic file is written from whatever the fetch
    /// returned, so the redaction above has to be where the text is made:
    /// this drives the real refresh path with a real app-server stub and
    /// reads the file back.
    #[cfg(unix)]
    #[test]
    fn the_recorded_diagnostic_never_holds_a_credential_from_an_rpc_error() {
        use std::io::Write;
        const SENTINEL: &str = "credential-sentinel-9f2c";
        let directory = tempfile::tempdir().unwrap();
        let stub = directory.path().join("codex-stub");
        let mut file = std::fs::File::create(&stub).unwrap();
        writeln!(
            file,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  printf '{{"jsonrpc":"2.0","id":%s,"error":{{"code":"{SENTINEL}","message":"denied for api_key=\\"{SENTINEL}\" and Authorization: Bearer {SENTINEL} via https://user:pw@proxy.example.com","data":{{"access_token":"{SENTINEL}"}}}}}}\n' "$id"
done"#
        )
        .unwrap();
        drop(file);
        let mut permissions = std::fs::metadata(&stub).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&stub, permissions).unwrap();

        let cache = crate::cache::CacheStore::new(directory.path().join("state"));
        let _guard = crate::providers::test_support::env_guard();
        let previous_bin = std::env::var_os("CODEX_BIN_PATH");
        let previous_home = std::env::var_os("CODEX_HOME");
        let previous_auth = std::env::var_os("CODEX_AUTH_FILE");
        std::env::set_var("CODEX_BIN_PATH", &stub);
        std::env::set_var("CODEX_HOME", directory.path().join("codex-home"));
        std::env::set_var(
            "CODEX_AUTH_FILE",
            directory.path().join("codex-home/auth.json"),
        );
        let outcome = crate::refresh::refresh_codex_for_test(&cache);
        for (name, previous) in [
            ("CODEX_BIN_PATH", previous_bin),
            ("CODEX_HOME", previous_home),
            ("CODEX_AUTH_FILE", previous_auth),
        ] {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let error = outcome.error.expect("the fetch must have failed");
        assert!(!error.contains(SENTINEL), "{error}");
        let recorded =
            std::fs::read_to_string(cache.root().join("codex-app-server.error.json")).unwrap();
        assert!(
            recorded.contains("Codex app-server request failed"),
            "{recorded}"
        );
        assert!(
            !recorded.contains(SENTINEL),
            "a credential reached the file: {recorded}"
        );
    }

    #[test]
    fn selects_codex_windows_by_duration_not_position() {
        let value = json!({
            "result": {"rateLimits": {
                "primary": {"usedPercent": 20.0, "windowDurationMins": 300, "resetsAt": 1786795200},
                "secondary": {"usedPercent": 61.0, "windowDurationMins": 10080, "resetsAt": 1787400000}
            }}
        });
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_787_400_000))
        );
    }

    #[test]
    fn accepts_a_codex_response_with_only_the_five_hour_window() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 20.0, "windowDurationMins": 300}
        }}});
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert!(snapshot.window(WindowKind::FiveHour).is_some());
    }

    #[test]
    fn rejects_codex_response_without_supported_windows() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 20.0, "windowDurationMins": 60}
        }}});
        assert!(parse_rate_limits(&value, 1).is_err());
    }

    #[test]
    fn maps_near_five_hour_header_durations_to_the_five_hour_window() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 12.0, "windowDurationMins": 299, "resetsAt": 1786795200},
            "secondary": {"usedPercent": 24.0, "windowDurationMins": 10079, "resetsAt": 1787400000}
        }}});
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert!(snapshot.window(WindowKind::FiveHour).is_some());
        assert!(snapshot.window(WindowKind::Weekly).is_some());
    }

    #[test]
    fn reads_a_five_hour_window_from_another_limit_id_bucket() {
        let value = json!({
            "result": {
                "rateLimits": {
                    "primary": {"usedPercent": 11.0, "windowDurationMins": 10080, "resetsAt": 1787400000},
                    "secondary": null
                },
                "rateLimitsByLimitId": {
                    "codex": {
                        "primary": {"usedPercent": 11.0, "windowDurationMins": 10080, "resetsAt": 1787400000},
                        "secondary": null
                    },
                    "codex_other": {
                        "primary": {"usedPercent": 40.0, "windowDurationMins": 300, "resetsAt": 1786795200},
                        "secondary": null
                    }
                }
            }
        });
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().used_percent,
            40.0
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            11.0
        );
    }

    #[test]
    fn reads_codex_account_id_from_local_auth_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"account_id":"acc-1","access_token":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(account_id_from_auth(&path).as_deref(), Some("acc-1"));
    }

    #[test]
    fn distinguishes_chatgpt_subscription_from_api_key() {
        assert!(account_is_chatgpt(
            &json!({"result": {"account": {"authMode": "chatgpt"}}})
        ));
        assert!(!account_is_chatgpt(
            &json!({"result": {"account": {"authMode": "api_key"}}})
        ));
    }

    #[test]
    fn extracts_compact_session_summaries_without_default_prompt() {
        let summaries = parse_session_summaries(&json!({
            "result": {"data": [
                {"id": "thread-1", "preview": "A real task\n\nmore detail"},
                {"id": "thread-2", "preview": "Ask Codex to do anything"}
            ]}
        }));
        assert_eq!(
            summaries.get("thread-1").map(String::as_str),
            Some("A real task")
        );
        assert!(!summaries.contains_key("thread-2"));
    }

    #[test]
    fn parses_rollout_model_context_and_session_cache() {
        let observation = parse_rollout_observation(
            &format!(
                "{}\n{}\n",
                serde_json::to_string(&json!({
                    "type": "turn_context",
                    "payload": {"model": "gpt-5.6-luna"}
                }))
                .unwrap(),
                serde_json::to_string(&json!({
                    "type": "event_msg",
                    "timestamp": "2026-08-26T02:28:42Z",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "last_token_usage": {
                                "total_tokens": 50_000,
                                "cached_input_tokens": 800,
                                "cache_write_input_tokens": 100
                            },
                            "total_token_usage": {
                                "input_tokens": 1_000,
                                "cached_input_tokens": 800,
                                "cache_write_input_tokens": 100
                            },
                            "model_context_window": 100_000
                        }
                    }
                }))
                .unwrap()
            ),
            "session-1",
        )
        .unwrap();
        assert_eq!(observation.model.as_deref(), Some("gpt-5.6-luna"));
        let context = observation.context.unwrap();
        assert!((context.used_percent - 43.1818).abs() < 0.001);
        let cache = context.cache.unwrap();
        assert_eq!(cache.fresh_input_tokens, 200);
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(cache.creation_tokens, 100);
        assert_eq!(cache.session_id.as_deref(), Some("session-1"));
        assert_eq!(cache.session_totals.unwrap().hit_percent, 72.72727272727273);
        assert_eq!(cache.ttl_seconds, Some(CODEX_PROMPT_CACHE_TTL_SECONDS));
        assert_eq!(cache.last_activity_unix, Some(1_787_711_322));
        assert_eq!(cache.expires_at_unix, None);
    }

    #[test]
    fn rollout_cache_without_a_timestamp_has_no_ttl_estimate() {
        let observation = parse_rollout_observation(
            &format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "last_token_usage": {"total_tokens": 50_000},
                            "total_token_usage": {
                                "input_tokens": 1_000,
                                "cached_input_tokens": 800
                            },
                            "model_context_window": 100_000
                        }
                    }
                }))
                .unwrap()
            ),
            "session-1",
        )
        .unwrap();
        let cache = observation.context.unwrap().cache.unwrap();
        assert_eq!(cache.ttl_seconds, None);
        assert_eq!(cache.last_activity_unix, None);
    }

    #[test]
    fn enriches_only_rollouts_matching_thread_ids() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/26");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string(), "other-session".to_string()],
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5.6"));
        assert!(snapshot.session_contexts.contains_key("session-1"));
        assert!(!snapshot.session_contexts.contains_key("other-session"));
    }

    #[test]
    fn unattributed_rollout_cannot_add_a_window_to_the_current_accounts_quota() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":24.0,"windowDurationMins":10080,"resetsAt":1787400000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        snapshot.account_id = Some("new-account".to_string());
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        // A file written after login may still be an old account's live
        // session. Even an identical reset timestamp does not prove identity.
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert!(snapshot.session_contexts.contains_key("session-1"));
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            24.0
        );
    }

    #[test]
    fn stale_rollout_five_hour_window_is_not_used_after_a_newer_weekly_only_event() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":99.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":49.0,"window_minutes":10080,"resets_at":1787400000}}}}
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":0.0,"window_minutes":10080,"resets_at":1787397000},"secondary":null},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":0.0,"windowDurationMins":10080,"resetsAt":1787397000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            0.0
        );
    }

    #[test]
    fn rollout_five_hour_window_is_not_used_without_account_identity() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":80.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":12.0,"windowDurationMins":10080,"resetsAt":1787400000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            12.0
        );
    }

    #[test]
    fn rollout_five_hour_window_is_not_used_when_weekly_reset_disagrees() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":80.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":12.0,"windowDurationMins":10080,"resetsAt":1787397000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            12.0
        );
    }

    #[test]
    fn reads_codex_account_id_from_chatgpt_account_id_when_account_id_is_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"chatgpt_account_id":"acc-2","access_token":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(account_id_from_auth(&path).as_deref(), Some("acc-2"));
    }

    fn token_count_pad_line() -> String {
        serde_json::to_string(&json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "total_tokens": 50_000,
                        "cached_input_tokens": 800,
                        "cache_write_input_tokens": 100
                    },
                    "total_token_usage": {
                        "input_tokens": 1_000,
                        "cached_input_tokens": 800,
                        "cache_write_input_tokens": 100
                    },
                    "model_context_window": 100_000
                }
            }
        }))
        .unwrap()
            + "\n"
    }

    fn pad_jsonl(body: &mut String, min_len: usize, pad_line: &str) {
        while body.len() < min_len {
            body.push_str(pad_line);
        }
    }

    fn write_padded_rollout(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    /// A long Codex turn writes `turn_context` at the start, then enough
    /// `token_count` lines to push that model out of the 256 KB tail. The
    /// session-start model is still in the first 256 KB. Publishing the head
    /// fallback is the stale-sidebar bug: context/cache keep moving, model does
    /// not.
    #[test]
    fn latest_turn_context_beyond_the_tail_wins_over_the_session_start_model() {
        let directory = tempfile::tempdir().unwrap();
        let pad = token_count_pad_line();
        let mut body = String::new();
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6-astra\"}}\n");
        pad_jsonl(&mut body, ROLLOUT_TAIL_BYTES as usize + pad.len(), &pad);
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\"}}\n");
        let tail_floor = body.len() + ROLLOUT_TAIL_BYTES as usize + pad.len();
        pad_jsonl(&mut body, tail_floor, &pad);
        write_padded_rollout(
            &directory
                .path()
                .join("sessions/2026/09/15/rollout-session-1.jsonl"),
            &body,
        );

        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(
            snapshot.session_models.get("session-1").map(String::as_str),
            Some("gpt-5.6-sol")
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5.6-sol"));
        assert!(snapshot.session_contexts.contains_key("session-1"));
    }

    #[test]
    fn session_start_model_is_kept_when_it_is_still_the_latest_turn_context() {
        let directory = tempfile::tempdir().unwrap();
        let pad = token_count_pad_line();
        let mut body = String::new();
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6-astra\"}}\n");
        pad_jsonl(&mut body, ROLLOUT_TAIL_BYTES as usize + pad.len(), &pad);
        write_padded_rollout(
            &directory
                .path()
                .join("sessions/2026/09/15/rollout-session-1.jsonl"),
            &body,
        );

        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(
            snapshot.session_models.get("session-1").map(String::as_str),
            Some("gpt-6-astra")
        );
    }
}

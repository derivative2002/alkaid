use crate::process::{run_shell_with_deadline, CommandOutput, STATUSLINE_COMMAND_BUDGET};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WRAPPER_STATE_MARKER: &str = "HERDR_PLUGIN_STATE_DIR='";

pub(crate) struct Adapter {
    pub label: &'static str,
    pub subcommand: &'static str,
    /// The pre-per-settings backup name. Kept for reading backups written by
    /// older plugin versions; new backups add a settings digest before `.json`.
    pub backup_file: &'static str,
    /// Environment variable that names this harness's settings file, e.g.
    /// `CLAUDE_SETTINGS_FILE`. It is resolved both by `configure` and by the
    /// statusLine hook, which is how several Claude HOMEs are told apart.
    pub settings_env: &'static str,
    /// Settings file relative to `$HOME` when the environment variable is unset.
    pub default_settings_relative: &'static str,
}

impl Adapter {
    pub fn check(&self, path: &Path, state: Option<&Path>) -> Result<()> {
        let settings = read_settings(path, self.label)?;
        if self.is_installed(settings.get("statusLine")) {
            println!(
                "{} statusLine collector is installed: {}",
                self.label,
                path.display()
            );
            if let Some(state) = state {
                let backup = self.backup_path(state, path);
                if backup.exists() {
                    println!(
                        "{} statusLine backup for this settings file: {}",
                        self.label,
                        backup.display()
                    );
                } else {
                    println!(
                        "{} has no backup for this settings file (expected {}); \
                         a re-install cannot restore what it replaced",
                        self.label,
                        backup.display()
                    );
                }
            }
        } else {
            println!(
                "{} statusLine preview for {}: install a reversible, silent quota collector",
                self.label,
                path.display()
            );
        }
        Ok(())
    }

    pub fn apply(&self, path: &Path, state: &Path, executable: &Path) -> Result<()> {
        self.apply_with_refresh_interval(path, state, executable, None)
    }

    pub fn apply_with_refresh_interval(
        &self,
        path: &Path,
        state: &Path,
        executable: &Path,
        refresh_interval_seconds: Option<u64>,
    ) -> Result<()> {
        let mut settings = read_settings(path, self.label)?;
        let installed = self.is_installed(settings.get("statusLine"));
        if !installed && !can_chain_statusline(settings.get("statusLine")) {
            anyhow::bail!(
                "existing {} statusLine has no safely chainable command; refusing to replace it",
                self.label
            );
        }
        fs::create_dir_all(state).context("create plugin state directory")?;
        let backup = self.backup_path(state, path);
        if !backup.exists() {
            let legacy = self.legacy_backup_path(state);
            if legacy.exists() {
                // A backup from before backups were keyed by settings file. It
                // is the only record of what this install replaced, so adopt it
                // under the settings-specific name rather than orphaning it.
                fs::rename(&legacy, &backup)
                    .with_context(|| format!("migrate {} statusLine backup", self.label))?;
                println!(
                    "{} migrated legacy statusLine backup {} -> {}",
                    self.label,
                    legacy.display(),
                    backup.display()
                );
            } else {
                let original = if installed {
                    self.previous_backup_from_wrapper(settings.get("statusLine"), path)?
                        .unwrap_or(Value::Null)
                } else {
                    settings.get("statusLine").cloned().unwrap_or(Value::Null)
                };
                fs::write(&backup, serde_json::to_vec_pretty(&original)?)
                    .with_context(|| format!("write {} statusLine backup", self.label))?;
            }
        }
        self.record_backup(state, path, &backup)?;
        let wrapper_command = format!(
            "{}={} {}={} {} {}",
            self.settings_env,
            shell_quote(path),
            "HERDR_PLUGIN_STATE_DIR",
            shell_quote(state),
            shell_quote(executable),
            self.subcommand
        );
        let status_line = settings
            .get_mut("statusLine")
            .and_then(Value::as_object_mut)
            .map(|object| {
                object.insert("type".to_string(), Value::String("command".to_string()));
                object.insert(
                    "command".to_string(),
                    Value::String(wrapper_command.clone()),
                );
                if let Some(seconds) = refresh_interval_seconds {
                    if installed || !object.contains_key("refreshInterval") {
                        object.insert("refreshInterval".to_string(), Value::from(seconds));
                    }
                }
                Value::Object(object.clone())
            })
            .unwrap_or_else(|| {
                let mut value = json!({"type": "command", "command": wrapper_command});
                if let Some(seconds) = refresh_interval_seconds {
                    value["refreshInterval"] = Value::from(seconds);
                }
                value
            });
        settings["statusLine"] = status_line;
        write_settings(path, &settings, self.label)
    }

    pub fn uninstall(&self, path: &Path, state: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut settings = read_settings(path, self.label)?;
        if !self.is_installed(settings.get("statusLine")) {
            return Ok(());
        }
        // Prefer the backup for this exact settings file; fall back to the
        // legacy single-name backup so an install made before this change can
        // still be reversed.
        let backup = {
            let scoped = self.backup_path(state, path);
            if scoped.exists() {
                scoped
            } else {
                self.legacy_backup_path(state)
            }
        };
        if !backup.exists() {
            println!(
                "{} statusLine for {} was not installed by this plugin \
                 (no backup at {}); leaving it untouched",
                self.label,
                path.display(),
                backup.display()
            );
            return Ok(());
        }
        let original: Value = serde_json::from_slice(&fs::read(&backup)?)?;
        if original.is_null() {
            settings
                .as_object_mut()
                .with_context(|| format!("{} settings must be an object", self.label))?
                .remove("statusLine");
        } else {
            settings["statusLine"] = original;
        }
        write_settings(path, &settings, self.label)?;
        fs::remove_file(&backup)
            .with_context(|| format!("remove {} statusLine backup", self.label))?;
        self.remove_backup_entry(state, path)?;
        Ok(())
    }

    pub fn previous_command(&self, state: &Path) -> Result<Option<String>> {
        // The first existing backup wins: a settings-scoped backup that records
        // "no previous command" must not fall through to a legacy file for a
        // different settings path.
        for backup in self.backup_candidates(state) {
            if !backup.exists() {
                continue;
            }
            let value: Value = serde_json::from_slice(&fs::read(&backup)?)?;
            return Ok(match value {
                Value::String(command) => Some(command),
                Value::Object(map) => map
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            });
        }
        Ok(None)
    }

    pub(crate) fn run_previous(&self, state: &Path, input: &[u8]) -> Result<Option<CommandOutput>> {
        let Some(command) = self.previous_command(state)? else {
            return Ok(None);
        };
        run_shell_with_deadline(&command, input, STATUSLINE_COMMAND_BUDGET).map(Some)
    }

    pub(crate) fn settings_path(&self) -> Result<PathBuf> {
        settings_path(self.settings_env, self.default_settings_relative)
    }

    /// Backups to look for, most specific first: the name derived from this
    /// settings path, then the pre-per-settings legacy name.
    fn backup_candidates(&self, state: &Path) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(settings) = self.settings_path() {
            candidates.push(self.backup_path(state, &settings));
        }
        candidates.push(self.legacy_backup_path(state));
        candidates
    }

    fn backup_name(&self, settings: &Path) -> String {
        let base = self
            .backup_file
            .strip_suffix(".json")
            .unwrap_or(self.backup_file);
        format!("{base}.{}.json", settings_digest8(settings))
    }

    fn backup_path(&self, state: &Path, settings: &Path) -> PathBuf {
        state.join(self.backup_name(settings))
    }

    fn legacy_backup_path(&self, state: &Path) -> PathBuf {
        state.join(self.backup_file)
    }

    /// `claude-statusline.original.json` -> `claude-statusline.backups.json`.
    fn index_name(&self) -> String {
        match self.backup_file.strip_suffix(".original.json") {
            Some(base) => format!("{base}.backups.json"),
            None => format!(
                "{}.backups.json",
                self.backup_file.trim_end_matches(".json")
            ),
        }
    }

    fn index_path(&self, state: &Path) -> PathBuf {
        state.join(self.index_name())
    }

    /// Record `settings path -> backup name -> takeover time` so the state
    /// directory explains which backup belongs to which Claude HOME.
    fn record_backup(&self, state: &Path, settings: &Path, backup: &Path) -> Result<()> {
        let path = self.index_path(state);
        let mut index = read_index(&path);
        let name = backup
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        index[settings.to_string_lossy().into_owned()] = json!({
            "backup": name,
            "taken_over_at_unix": now_unix(),
        });
        write_index(&path, &index)
    }

    fn remove_backup_entry(&self, state: &Path, settings: &Path) -> Result<()> {
        let path = self.index_path(state);
        let mut index = read_index(&path);
        if let Some(object) = index.as_object_mut() {
            object.remove(&settings.to_string_lossy().into_owned());
        }
        write_index(&path, &index)
    }

    fn is_installed(&self, status_line: Option<&Value>) -> bool {
        status_line
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.contains(self.subcommand)
                    && (command.contains("herdr-agent-quota")
                        || command.contains("agy-statusline.sh"))
            })
    }

    /// When an older wrapper already points at a state directory, the original
    /// statusLine lives there. Look for this settings file's backup first, then
    /// the legacy single-name backup the old plugin wrote.
    fn previous_backup_from_wrapper(
        &self,
        status_line: Option<&Value>,
        settings: &Path,
    ) -> Result<Option<Value>> {
        let Some(command) = status_line
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(old_state) = state_dir_from_wrapper(command) else {
            return Ok(None);
        };
        for candidate in [
            self.backup_path(old_state, settings),
            self.legacy_backup_path(old_state),
        ] {
            if candidate.exists() {
                return Ok(Some(serde_json::from_slice(&fs::read(&candidate)?)?));
            }
        }
        Ok(None)
    }
}

fn settings_digest8(settings: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(settings.to_string_lossy().as_bytes())
    )[..8]
        .to_string()
}

/// Pull `HERDR_PLUGIN_STATE_DIR='<dir>'` out of a wrapper command. The dir is
/// found anywhere in the command, so both the legacy prefix form and the
/// current form (which leads with the settings-file variable) parse.
fn state_dir_from_wrapper(command: &str) -> Option<&Path> {
    let start = command.find(WRAPPER_STATE_MARKER)? + WRAPPER_STATE_MARKER.len();
    let rest = &command[start..];
    let end = rest.find('\'')?;
    Some(Path::new(&rest[..end]))
}

fn read_index(path: &Path) -> Value {
    let parsed = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match parsed {
        Some(value) if value.is_object() => value,
        _ => json!({}),
    }
}

fn write_index(path: &Path, index: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(index)?)
        .with_context(|| format!("write {} backups index", path.display()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn can_chain_statusline(status_line: Option<&Value>) -> bool {
    match status_line {
        None | Some(Value::Null) | Some(Value::String(_)) => true,
        Some(Value::Object(map)) => map
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        Some(_) => false,
    }
}

pub(crate) fn settings_path(environment: &str, relative: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(environment) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(relative))
}

fn read_settings(path: &Path, label: &str) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let value: Value =
        serde_json::from_slice(&fs::read(path).with_context(|| format!("read {label} settings"))?)
            .with_context(|| format!("parse {label} settings"))?;
    if !value.is_object() {
        anyhow::bail!("{label} settings must be a JSON object")
    }
    Ok(value)
}

fn write_settings(path: &Path, settings: &Value, label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {label} settings directory"))?;
    }
    let temporary = path.with_extension("json.herdr-agent-quota.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(settings)?)?;
    fs::rename(temporary, path).with_context(|| format!("replace {label} settings"))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

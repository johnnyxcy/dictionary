use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
};

use crate::{
    model::{Setup, SkillSnapshot, now},
    runner::ProcessGroup,
    settings::{Catalog, ModelOption, SkillSource},
};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub data: PathBuf,
    pub database: PathBuf,
    pub workspace: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let data = match std::env::var_os("DICTIONARY_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => dirs::data_dir()
                .context("Cannot locate Application Support")?
                .join("Dictionary"),
        };
        Self::at(data)
    }

    pub fn at(data: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data)?;
        let data = data.canonicalize()?;
        let workspace = data.join("runtime");
        std::fs::create_dir_all(&workspace)?;
        Ok(Self {
            database: data.join("history.sqlite3"),
            data,
            workspace,
        })
    }
}

pub fn find_executable(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DICTIONARY_CLI")
        .map(PathBuf::from)
        .or_else(|| configured.map(Path::to_owned))
    {
        validate_executable(&path)?;
        // Keep symlink names: some CLI launchers use argv[0] to select a command.
        return Ok(path);
    }
    let mut candidates = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .map(|p| p.join("codex"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ]);
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/codex"));
    }
    candidates.into_iter().find(|p| is_executable(p)).context("Codex was not found. In Terminal, run `command -v codex`, then choose that executable in Settings → CLI location. Install the CLI first if the command has no result.")
}

pub fn validate_executable(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "Choose an absolute CLI executable path in Settings"
    );
    ensure!(
        is_executable(path),
        "The CLI at {} is missing or is not executable. Choose its current location in Settings.",
        path.display()
    );
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Launch without sourcing a shell. Adjacent runtimes support installations
/// such as npm/nvm whose launcher uses /usr/bin/env even when Finder has a small PATH.
pub(crate) fn cli_command(executable: &Path) -> Result<Command> {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = Vec::new();
    let mut link = executable.to_owned();
    for _ in 0..16 {
        if let Some(parent) = link.parent() {
            paths.push(parent.to_owned());
        }
        let Ok(target) = std::fs::read_link(&link) else {
            break;
        };
        link = if target.is_absolute() {
            target
        } else {
            link.parent().unwrap_or(Path::new("/")).join(target)
        };
    }
    paths.extend(std::env::split_paths(&inherited).filter(|p| p.is_absolute()));
    for path in [
        Path::new("/opt/homebrew/bin"),
        Path::new("/usr/local/bin"),
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ] {
        if !paths.iter().any(|p| p == path) {
            paths.push(path.to_owned());
        }
    }
    let mut command = Command::new(executable);
    command.env("PATH", std::env::join_paths(paths)?);
    Ok(command)
}

pub async fn discover_catalog(workspace: PathBuf, configured: Option<PathBuf>) -> Result<Catalog> {
    tokio::time::timeout(
        Duration::from_secs(45),
        discover_inner(workspace, configured),
    )
    .await
    .context("Codex setup timed out. Saved history is still available; reconnect to try again.")?
}

async fn discover_inner(workspace: PathBuf, configured: Option<PathBuf>) -> Result<Catalog> {
    let executable = find_executable(configured.as_deref())?;
    let mut version_command = cli_command(&executable)?;
    version_command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, mut version_group) =
        ProcessGroup::spawn(&mut version_command).with_context(|| {
            format!(
                "Cannot start {}. Choose a working CLI executable in Settings.",
                executable.display()
            )
        })?;
    let version = child.wait_with_output().await?;
    version_group.terminate();
    version_group.disarm();
    ensure!(
        version.status.success(),
        "The CLI at {} did not respond to --version. Check that this is the Codex executable and that its runtime is installed. {}",
        executable.display(),
        String::from_utf8_lossy(&version.stderr).trim()
    );
    let cli_version = String::from_utf8(version.stdout)?.trim().to_owned();
    let mut command = cli_command(&executable)?;
    command
        .args(["app-server", "--stdio"])
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, mut group) =
        ProcessGroup::spawn(&mut command).context("Cannot start Codex metadata discovery")?;
    let mut rpc = Rpc {
        input: child.stdin.take().context("Missing metadata input pipe")?,
        output: BufReader::new(
            child
                .stdout
                .take()
                .context("Missing metadata output pipe")?,
        ),
        sequence: 0,
    };
    let result = async {
        rpc.call("initialize", json!({"clientInfo":{"name":"dictionary","title":"Dictionary","version":env!("CARGO_PKG_VERSION")}})).await?;
        rpc.input.write_all(b"{\"method\":\"initialized\"}\n").await?;
        rpc.input.flush().await?;
        let config = rpc.call("config/read", json!({"includeLayers":false})).await?;
        let configured_model = std::env::var("DICTIONARY_MODEL").ok().or_else(|| config.pointer("/config/model").and_then(Value::as_str).map(str::to_owned));
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        for _ in 0..20 {
            let page = rpc.call("model/list", json!({"limit":100,"includeHidden":false,"cursor":cursor})).await?;
            models.extend(page["data"].as_array().context("Codex returned an invalid model catalog")?.iter().cloned());
            cursor = page["nextCursor"].clone();
            if cursor.is_null() { break; }
        }
        ensure!(cursor.is_null(), "Model catalog pagination did not complete");
        let models = parse_models(&models)?;
        let sources = if let Some(path) = std::env::var_os("DICTIONARY_SKILL_PATH") {
            skill_source(PathBuf::from(path), "dict".into()).map(|source| vec![source])
        } else {
            rpc.call("skills/list", json!({"cwds":[workspace],"forceReload":true})).await.and_then(|response| skill_sources(&response))
        };
        // Keep model selection and the file-picker fallback available even if
        // automatic skill discovery fails (for example after a plugin update).
        let (skills, skill_diagnostic) = match sources {
            Ok(skills) => (skills, None),
            Err(error) => (Vec::new(), Some(format!("Dictionary skill discovery failed: {error:#}. Choose SKILL.md with Browse."))),
        };
        Ok(Catalog { executable, cli_version, retrieved_at: now(), configured_model, models, skills, skill_diagnostic })
    }.await;
    group.terminate();
    let _ = child.kill().await;
    let _ = child.wait().await;
    group.disarm();
    result
}

pub fn parse_models(values: &[Value]) -> Result<Vec<ModelOption>> {
    let mut models = Vec::new();
    for value in values {
        if value["hidden"] == true
            || value["inputModalities"]
                .as_array()
                .is_some_and(|modes| !modes.iter().any(|mode| mode == "text"))
        {
            continue;
        }
        let model = value["model"]
            .as_str()
            .or_else(|| value["id"].as_str())
            .context("Model has no identifier")?;
        models.push(ModelOption {
            id: value["id"].as_str().unwrap_or(model).into(),
            model: model.into(),
            display_name: value["displayName"].as_str().unwrap_or(model).into(),
            efforts: value["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|effort| effort["reasoningEffort"].as_str().map(str::to_owned))
                .collect(),
            default_effort: value["defaultReasoningEffort"]
                .as_str()
                .unwrap_or_default()
                .into(),
            is_default: value["isDefault"] == true,
            supports_fast: value["serviceTiers"].as_array().map_or_else(
                || {
                    value["additionalSpeedTiers"]
                        .as_array()
                        .is_some_and(|tiers| tiers.iter().any(|tier| tier == "fast"))
                },
                |tiers| {
                    tiers
                        .iter()
                        .any(|tier| matches!(tier["id"].as_str(), Some("priority" | "fast")))
                },
            ),
        });
    }
    ensure!(
        !models.is_empty(),
        "No text models are available. Run `codex login` in Terminal, then refresh Settings."
    );
    Ok(models)
}

fn skill_source(path: PathBuf, name: String) -> Result<SkillSource> {
    let setup = Setup {
        executable: PathBuf::new(),
        cli_version: String::new(),
        model: String::new(),
        effort: String::new(),
        service_tier: None,
        skill_path: path.clone(),
        skill_name: name.clone(),
    };
    let snapshot = SkillSnapshot::read(&setup)
        .with_context(|| format!("Cannot read dictionary skill at {}", path.display()))?;
    Ok(SkillSource {
        path,
        name,
        hash: snapshot.hash,
    })
}

pub fn skill_sources(response: &Value) -> Result<Vec<SkillSource>> {
    let mut candidates = Vec::new();
    for entry in response["data"].as_array().into_iter().flatten() {
        for skill in entry["skills"].as_array().into_iter().flatten() {
            if skill["enabled"] == false {
                continue;
            }
            let Some(name) = skill["name"].as_str() else {
                continue;
            };
            let rank = if name == "dict" || name.ends_with(":dict") {
                0
            } else if name == "en-zh-dictionary" {
                1
            } else {
                continue;
            };
            if let Some(path) = skill["path"].as_str() {
                candidates.push((rank, name.to_owned(), PathBuf::from(path)));
            }
        }
    }
    candidates.sort();
    let Some(rank) = candidates.first().map(|c| c.0) else {
        return Ok(Vec::new());
    };
    let mut unique = BTreeMap::new();
    for (_, name, path) in candidates.into_iter().filter(|c| c.0 == rank) {
        let source = skill_source(path, name)?;
        unique.entry(source.hash.clone()).or_insert(source);
    }
    Ok(unique.into_values().collect())
}

struct Rpc {
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    sequence: u32,
}

impl Rpc {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let mut request = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))?;
        request.push(b'\n');
        self.input.write_all(&request).await?;
        self.input.flush().await?;
        loop {
            let mut line = String::new();
            ensure!(
                self.output.read_line(&mut line).await? != 0,
                "Codex metadata service exited during {method}"
            );
            ensure!(
                line.len() <= 4 * 1024 * 1024,
                "Metadata response exceeds the size limit"
            );
            let response: Value =
                serde_json::from_str(&line).context("Invalid Codex metadata response")?;
            if response["id"] != id {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!(
                    "Codex {method}: {}",
                    error["message"]
                        .as_str()
                        .unwrap_or("metadata request failed")
                );
            }
            return response
                .get("result")
                .cloned()
                .context("Codex metadata response has no result");
        }
    }
}

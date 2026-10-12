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
    model::{Setup, SkillSnapshot},
    runner::ProcessGroup,
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

pub fn find_executable() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DICTIONARY_CLI") {
        let path = PathBuf::from(path);
        ensure!(
            is_executable(&path),
            "DICTIONARY_CLI must point to an executable file"
        );
        // Keep symlink names: some CLI launchers use argv[0] to select a command.
        return Ok(path);
    }
    let mut candidates = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
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
    candidates.into_iter().find(|p| is_executable(p)).context("Codex was not found. Install the Codex CLI, or set DICTIONARY_CLI to its absolute executable path, then reconnect.")
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

pub async fn discover(workspace: PathBuf) -> Result<Setup> {
    tokio::time::timeout(Duration::from_secs(45), discover_inner(workspace))
        .await
        .context(
            "Codex setup timed out. Saved history is still available; reconnect to try again.",
        )?
}

async fn discover_inner(workspace: PathBuf) -> Result<Setup> {
    let executable = find_executable()?;
    let version = Command::new(&executable)
        .arg("--version")
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        version.status.success(),
        "The configured Codex executable did not respond to --version"
    );
    let cli_version = String::from_utf8(version.stdout)?.trim().to_owned();
    let mut command = Command::new(&executable);
    command
        .args(["app-server", "--stdio"])
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, group) =
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
        let selected = select_model(&models, configured_model.as_deref())?;
        let model = selected["model"].as_str().or_else(|| selected["id"].as_str()).context("Model has no identifier")?.to_owned();
        let efforts = selected["supportedReasoningEfforts"].as_array().context("The selected model does not advertise reasoning efforts")?;
        let effort = if efforts.iter().any(|e| e["reasoningEffort"] == "low") { "low" } else {
            selected["defaultReasoningEffort"].as_str().context("No supported default reasoning effort")?
        }.to_owned();
        ensure!(efforts.iter().any(|e| e["reasoningEffort"] == effort), "The model's default reasoning effort is not supported");
        let skills = rpc.call("skills/list", json!({"cwds":[workspace],"forceReload":true})).await?;
        let (skill_path, skill_name) = if let Some(path) = std::env::var_os("DICTIONARY_SKILL_PATH") {
            (PathBuf::from(path), "dict".to_owned())
        } else { select_skill(&skills)? };
        let setup = Setup { executable, cli_version, model, effort, skill_path, skill_name };
        SkillSnapshot::read(&setup).context("Cannot read the selected dictionary skill")?;
        Ok(setup)
    }.await;
    group.terminate();
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

pub fn select_model<'a>(models: &'a [Value], configured: Option<&str>) -> Result<&'a Value> {
    if let Some(name) = configured {
        if let Some(model) = models
            .iter()
            .find(|m| m["model"] == name || m["id"] == name)
        {
            return Ok(model);
        }
        bail!(
            "Configured model '{name}' is not in the available catalog. Choose an available model in Codex, or set DICTIONARY_MODEL, then reconnect."
        );
    }
    models
        .iter()
        .find(|m| m["isDefault"] == true)
        .or_else(|| models.first())
        .context("No models are available. Run `codex login` in Terminal, then reconnect.")
}

fn select_skill(response: &Value) -> Result<(PathBuf, String)> {
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
    let rank = candidates.first().context("No local dictionary skill was found. Install your dict skill or set DICTIONARY_SKILL_PATH to its SKILL.md file, then reconnect.")?.0;
    let mut unique = BTreeMap::new();
    for (_, name, path) in candidates.into_iter().filter(|c| c.0 == rank) {
        let setup = Setup {
            executable: PathBuf::new(),
            cli_version: String::new(),
            model: String::new(),
            effort: String::new(),
            skill_path: path.clone(),
            skill_name: name.clone(),
        };
        let snapshot = SkillSnapshot::read(&setup)
            .with_context(|| format!("Cannot read dictionary skill at {}", path.display()))?;
        unique.entry(snapshot.hash).or_insert((path, name));
    }
    ensure!(
        unique.len() == 1,
        "Different copies of the dict skill are installed. Set DICTIONARY_SKILL_PATH to the copy you want, then reconnect."
    );
    unique
        .into_values()
        .next()
        .context("No readable dictionary skill was found")
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

use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, watch},
};

use crate::{
    model::{Activity, Attempt, AttemptStatus, Failure, now},
    protocol::{JsonlDecoder, TurnTracker},
};

#[derive(Clone, Debug)]
pub struct Progress {
    pub attempt_id: String,
    pub answer: String,
    pub activity: Vec<Activity>,
}

/// Dedicated process group: cancellation never targets the user's shared daemon.
pub(crate) struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    pub(crate) fn spawn(command: &mut Command) -> Result<(Child, Self)> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        command.kill_on_drop(true);
        let child = command.spawn()?;
        let group = Self(child.id());
        Ok((child, group))
    }

    pub(crate) fn terminate(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // The group was created by this application and is scoped to this child.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub async fn run(
    mut attempt: Attempt,
    prompt: String,
    working_directory: &std::path::Path,
    mut cancellation: watch::Receiver<bool>,
    progress: async_channel::Sender<Progress>,
    timeout: Duration,
    submitted: Instant,
) -> Attempt {
    let mut command = Command::new(&attempt.setup.executable);
    command
        .args([
            "--no-daemon",
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "--color",
            "never",
            "--sandbox",
            "read-only",
            "--model",
        ])
        .arg(&attempt.setup.model)
        .arg("-c")
        .arg("approval_policy=\"never\"")
        .arg("-c")
        .arg("model_reasoning_summary=\"auto\"")
        .arg("-c")
        .arg("hide_agent_reasoning=false")
        .arg("-c")
        .arg(format!(
            "model_reasoning_effort={}",
            serde_json::to_string(&attempt.setup.effort).expect("string serialization")
        ))
        .arg("-")
        .current_dir(working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let cli_started = Instant::now();
    attempt.queue_duration_ms = Some(millis(submitted));
    let (mut child, mut group) = match ProcessGroup::spawn(&mut command) {
        Ok(value) => value,
        Err(error) => {
            attempt.status = AttemptStatus::Failed;
            attempt.failure = Some(Failure::new(
                "spawn_failed",
                format!("Cannot start Codex: {error}. Check the executable path."),
            ));
            attempt.completed_at = Some(now());
            attempt.duration_ms = Some(millis(submitted));
            return attempt;
        }
    };
    attempt.started_at = Some(now());
    attempt.status = AttemptStatus::Running;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut writer = tokio::spawn(async move {
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.shutdown().await
    });
    let (events, mut receiver) = mpsc::channel(32);
    let reader = tokio::spawn(read_events(stdout, events));
    let diagnostics = tokio::spawn(read_tail(stderr));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut tracker = TurnTracker::default();
    let mut exit = None;
    let mut eof = false;
    let mut forced = None;
    let mut previous_answer = String::new();
    let mut previous_activity = Vec::new();

    loop {
        if eof && exit.is_some() {
            break;
        }
        if *cancellation.borrow() {
            forced = Some((
                AttemptStatus::Cancelled,
                Failure::new("cancelled", "Lookup canceled"),
            ));
            break;
        }
        tokio::select! {
            _ = cancellation.changed() => {
                forced = Some((AttemptStatus::Cancelled, Failure::new("cancelled", "Lookup canceled")));
                break;
            }
            _ = &mut deadline => {
                forced = Some((AttemptStatus::TimedOut, Failure::new("timeout", "The lookup exceeded its time limit. You can submit it again.")));
                break;
            }
            status = child.wait(), if exit.is_none() => {
                match status {
                    Ok(status) => { exit = Some(status); attempt.cli_duration_ms = Some(millis(cli_started)); }
                    Err(error) => { forced = Some((AttemptStatus::Failed, Failure::new("process_wait", error.to_string()))); break; }
                }
            }
            event = receiver.recv(), if !eof => {
                match event {
                    None => eof = true,
                    Some(Err(error)) => { forced = Some((AttemptStatus::Failed, Failure::new("protocol", error.to_string()))); break; }
                    Some(Ok(event)) => {
                        if let Err(error) = tracker.apply(event) {
                            forced = Some((AttemptStatus::Failed, Failure::new("protocol", error.to_string())));
                            break;
                        }
                        let answer = tracker.answer(false);
                        if answer != previous_answer || tracker.activity != previous_activity {
                            if !answer.is_empty() { attempt.first_answer_ms.get_or_insert_with(|| millis(submitted)); }
                            previous_answer = answer.clone();
                            previous_activity = tracker.activity.clone();
                            // Intermediate snapshots can be dropped under backpressure;
                            // the final persisted answer is always delivered separately.
                            let _ = progress.try_send(Progress { attempt_id: attempt.id.clone(), answer, activity: tracker.activity.clone() });
                        }
                    }
                }
            }
        }
    }

    if forced.is_some() {
        group.terminate();
        let _ = child.kill().await;
        if let Ok(status) = child.wait().await {
            exit = Some(status);
        }
        reader.abort();
    }
    // Closing owned descendants also releases a stdin reader inherited by a
    // child that outlived the CLI. A successful answer still requires that the
    // complete prompt was delivered.
    group.terminate();
    let prompt_error = if forced.is_some() {
        writer.abort();
        let _ = writer.await;
        None
    } else {
        match tokio::time::timeout(Duration::from_secs(2), &mut writer).await {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(error))) => Some(format!(
                "Cannot send the complete lookup to the CLI: {error}"
            )),
            Ok(Err(error)) => Some(format!("Lookup input worker failed: {error}")),
            Err(_) => {
                writer.abort();
                Some("The CLI stopped before accepting the complete lookup".into())
            }
        }
    };
    let _ = reader.await;
    let mut diagnostics = diagnostics;
    let stderr = match tokio::time::timeout(Duration::from_secs(2), &mut diagnostics).await {
        Ok(Ok(text)) => text,
        _ => {
            group.terminate();
            diagnostics.abort();
            String::new()
        }
    };
    // Reap the leader before disarming; clean up any owned lingering descendants.
    group.terminate();
    group.disarm();
    attempt.exit_code = exit.as_ref().and_then(std::process::ExitStatus::code);
    attempt.thread_id = tracker.thread_id.clone();
    attempt.reported_model = tracker.reported_model.clone();
    attempt.usage = tracker.usage.clone();
    attempt.answer = tracker.answer(false);
    attempt.activity = tracker.activity.clone();
    attempt.completed_at = Some(now());
    attempt
        .cli_duration_ms
        .get_or_insert_with(|| millis(cli_started));
    attempt.duration_ms = Some(millis(submitted));

    if let Some((status, failure)) = forced {
        attempt.status = status;
        attempt.failure = Some(failure);
    } else if let Some(failure) = tracker.failure {
        attempt.status = AttemptStatus::Failed;
        attempt.failure = Some(actionable_failure(&failure.kind, failure.message));
    } else if !exit.is_some_and(|status| status.success()) {
        let detail = tracker.last_error.unwrap_or_else(|| {
            if stderr.trim().is_empty() {
                "The CLI exited unsuccessfully".into()
            } else {
                stderr.trim().to_owned()
            }
        });
        attempt.status = AttemptStatus::Failed;
        attempt.failure = Some(actionable_failure("process_exit", detail));
    } else if let Some(error) = prompt_error {
        attempt.status = AttemptStatus::Failed;
        attempt.failure = Some(Failure::new("stdin", error));
    } else if !tracker.completed || tracker.answer(true).is_empty() {
        attempt.status = AttemptStatus::Failed;
        attempt.failure = Some(Failure::new(
            "incomplete_response",
            "The CLI exited without a complete dictionary response. Partial text and reported usage were retained.",
        ));
    } else {
        attempt.status = AttemptStatus::Succeeded;
        attempt.answer = tracker.answer(true);
    }
    attempt
}

fn actionable_failure(kind: &str, message: String) -> Failure {
    let lower = message.to_lowercase();
    let auth = ["login", "log in", "unauthorized", "authentication", "401"]
        .iter()
        .any(|s| lower.contains(s));
    if auth {
        Failure::new(
            "authentication",
            format!("{message}\nRun `codex login` in Terminal, then try again."),
        )
    } else {
        Failure::new(kind, message)
    }
}

async fn read_events(mut stream: impl AsyncRead + Unpin, sender: mpsc::Sender<Result<Value>>) {
    let mut decoder = JsonlDecoder::default();
    let mut buffer = [0; 8192];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => {
                match decoder.finish() {
                    Ok(Some(event)) => {
                        let _ = sender.send(Ok(event)).await;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                    }
                    _ => {}
                }
                break;
            }
            Ok(count) => {
                for event in decoder.push(&buffer[..count]) {
                    let failed = event.is_err();
                    if sender.send(event).await.is_err() || failed {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender
                    .send(Err(error).context("Cannot read CLI output"))
                    .await;
                break;
            }
        }
    }
}

async fn read_tail(mut stream: impl AsyncRead + Unpin) -> String {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    while let Ok(count) = stream.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 8192 {
            tail.drain(..tail.len() - 8192);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

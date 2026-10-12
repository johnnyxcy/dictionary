//! The version-tolerant JSONL boundary. Unknown events are ignored; malformed
//! JSON and contradictory token counters are explicit protocol failures.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::model::{Activity, Failure, Usage};

pub const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub struct JsonlDecoder {
    pending: Vec<u8>,
}

impl JsonlDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Value>> {
        let mut events = Vec::new();
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if self.pending.len() + part.len() > MAX_EVENT_BYTES {
                events.push(Err(anyhow!("CLI event exceeds the 2 MiB limit")));
                break;
            }
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                match self.parse_pending() {
                    Ok(Some(event)) => events.push(Ok(event)),
                    Ok(None) => {}
                    Err(error) => {
                        events.push(Err(error));
                        break;
                    }
                }
            }
        }
        events
    }

    pub fn finish(&mut self) -> Result<Option<Value>> {
        self.parse_pending()
            .context("CLI output ended with incomplete JSON")
    }

    fn parse_pending(&mut self) -> Result<Option<Value>> {
        let bytes = std::mem::take(&mut self.pending);
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_slice(&bytes).context("Invalid CLI JSON event")?,
        ))
    }
}

#[derive(Default)]
struct AnswerItem {
    id: String,
    text: String,
    phase: Option<String>,
    completed: bool,
}

#[derive(Default)]
pub struct TurnTracker {
    pub thread_id: Option<String>,
    pub reported_model: Option<String>,
    pub usage: Usage,
    pub completed: bool,
    pub failure: Option<Failure>,
    pub last_error: Option<String>,
    pub activity: Vec<Activity>,
    answers: Vec<AnswerItem>,
}

impl TurnTracker {
    pub fn apply(&mut self, event: Value) -> Result<()> {
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                self.thread_id = event
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.reported_model = event
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some(kind @ ("item.started" | "item.updated" | "item.completed")) => {
                let item = &event["item"];
                let completed = kind == "item.completed";
                self.record_item_activity(item, completed);
                if item["type"] == "agent_message" {
                    let id = item["id"].as_str().context("Agent message has no ID")?;
                    let index = self
                        .answers
                        .iter()
                        .position(|a| a.id == id)
                        .unwrap_or_else(|| {
                            self.answers.push(AnswerItem {
                                id: id.into(),
                                ..Default::default()
                            });
                            self.answers.len() - 1
                        });
                    let answer = &mut self.answers[index];
                    // Updated text is a snapshot, not a token delta. Do not let a
                    // delayed nonterminal update overwrite a completed item.
                    if !answer.completed || kind == "item.completed" {
                        if let Some(text) = item["text"].as_str() {
                            answer.text = text.to_owned();
                        }
                        if let Some(phase) = item["phase"].as_str() {
                            answer.phase = Some(phase.into());
                        }
                        answer.completed = kind == "item.completed";
                    }
                }
            }
            Some("turn.completed") => {
                self.completed = true;
                for item in &mut self.activity {
                    item.completed = true;
                }
                if let Some(usage) = event.get("usage").filter(|v| !v.is_null()) {
                    let usage: Usage = serde_json::from_value(usage.clone())?;
                    usage.validate()?;
                    // A terminal snapshot replaces previous counters. Replayed
                    // terminal events never increase the totals.
                    self.usage = usage;
                }
            }
            Some("turn.failed") => {
                self.failure = Some(Failure::new("turn_failed", error_message(&event)));
            }
            Some("error") => self.last_error = Some(error_message(&event)),
            _ => {}
        }
        Ok(())
    }

    fn record_activity(&mut self, id: &str, label: &str, text: String, completed: bool) {
        if text.trim().is_empty() {
            if completed && let Some(previous) = self.activity.iter_mut().find(|a| a.id == id) {
                previous.completed = true;
            }
            return;
        }
        // Preserve bounded snapshots, not repeated copies of every delta.
        let activity = Activity {
            id: id.into(),
            label: label.into(),
            text: text.chars().take(8_000).collect(),
            completed,
        };
        if let Some(previous) = self.activity.iter_mut().find(|a| a.id == id) {
            if !previous.completed || completed {
                *previous = activity;
            }
        } else {
            if self.activity.len() == 100 {
                self.activity.remove(0);
            }
            self.activity.push(activity);
        }
    }

    fn record_item_activity(&mut self, item: &Value, completed: bool) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        let (label, text) = match item["type"].as_str() {
            Some("reasoning") => (
                "Thinking",
                item["text"].as_str().unwrap_or_default().to_owned(),
            ),
            Some("agent_message") if item["phase"] == "commentary" => (
                "Working",
                item["text"].as_str().unwrap_or_default().to_owned(),
            ),
            Some("command_execution") => {
                let mut details = Vec::new();
                if let Some(command) = item["command"].as_str().filter(|s| !s.is_empty()) {
                    details.push(format!("$ {command}"));
                }
                if let Some(output) = item["aggregated_output"].as_str().filter(|s| !s.is_empty()) {
                    details.push(output.into());
                }
                if let Some(code) = item["exit_code"].as_i64() {
                    details.push(format!("Exit code: {code}"));
                }
                ("Command", details.join("\n"))
            }
            Some("mcp_tool_call") => {
                let mut details = [item["server"].as_str(), item["tool"].as_str()]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ");
                if let Some(error) = item.pointer("/error/message").and_then(Value::as_str) {
                    details.push_str(&format!("\n{error}"));
                }
                ("Tool", details)
            }
            Some("web_search") => (
                "Web search",
                item["query"].as_str().unwrap_or_default().to_owned(),
            ),
            Some("todo_list") => (
                "Plan",
                item["items"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|i| {
                                i["text"].as_str().filter(|s| !s.is_empty()).map(|text| {
                                    format!(
                                        "{} {text}",
                                        if i["completed"] == true { "✓" } else { "•" }
                                    )
                                })
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            ),
            Some("file_change") => (
                "File changes",
                item["changes"]
                    .as_array()
                    .map(|changes| {
                        changes
                            .iter()
                            .filter_map(|change| {
                                change["path"]
                                    .as_str()
                                    .map(|path| match change["kind"].as_str() {
                                        Some(kind) => format!("{kind}: {path}"),
                                        None => path.to_owned(),
                                    })
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            ),
            _ => return,
        };
        self.record_activity(&format!("item:{id}"), label, text, completed);
    }

    pub fn answer(&self, completed_only: bool) -> String {
        let eligible = |a: &&AnswerItem| {
            (!completed_only || a.completed)
                && a.phase.as_deref() != Some("commentary")
                && !a.text.trim().is_empty()
        };
        self.answers
            .iter()
            .rev()
            .filter(eligible)
            .find(|a| matches!(a.phase.as_deref(), Some("final_answer" | "final")))
            .or_else(|| self.answers.iter().rev().find(eligible))
            .map(|a| a.text.clone())
            .unwrap_or_default()
    }
}

fn error_message(event: &Value) -> String {
    event
        .pointer("/error/message")
        .or_else(|| event.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("The CLI reported a request failure")
        .into()
}

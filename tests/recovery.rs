//! Exercise an actual service-process crash with an owned local CLI fixture.
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use dictionary::{
    direction::DirectionChoice,
    discovery::AppPaths,
    model::{Attempt, AttemptStatus},
    service::{Command as ServiceCommand, Event, Service},
};

fn publish(path: &Path, attempt: &Attempt) {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(attempt).unwrap()).unwrap();
    std::fs::rename(temporary, path).unwrap();
}

#[test]
#[ignore = "subprocess entry point for crash recovery tests"]
fn recovery_child() {
    let root = std::env::var_os("DICTIONARY_RECOVERY_TEST_PROFILE").expect("recovery test profile");
    let root = PathBuf::from(root);
    let restarting = std::env::var("DICTIONARY_RECOVERY_TEST_MODE").unwrap() == "restart";
    let input = std::env::var("DICTIONARY_RECOVERY_TEST_INPUT").unwrap();
    let expect_usage = input == "crash after usage fixture";
    let (service, events) = Service::start(AppPaths::at(root.clone()).unwrap()).unwrap();
    let mut submitted = false;
    while let Ok(event) = events.recv_blocking() {
        match event {
            Event::Ready(_) if !restarting && !submitted => {
                submitted = true;
                service
                    .send(ServiceCommand::Lookup {
                        request_id: 1,
                        input: input.clone(),
                        direction: DirectionChoice::EnZh,
                    })
                    .unwrap();
            }
            Event::Progress(progress)
                if !restarting
                    && !progress.answer.is_empty()
                    && (!expect_usage || progress.metadata.usage.total().is_some()) =>
            {
                // The service publishes progress only after its checkpoint.
                service
                    .send(ServiceCommand::Open {
                        request_id: 2,
                        attempt_id: progress.attempt_id,
                    })
                    .unwrap();
            }
            Event::Opened {
                request_id: 2,
                attempt,
            } if !restarting => publish(&root.join("checkpoint.json"), &attempt),
            Event::History(entries) if restarting => {
                let entry = entries.first().expect("history survives process death");
                service
                    .send(ServiceCommand::Open {
                        request_id: 3,
                        attempt_id: entry.id.clone(),
                    })
                    .unwrap();
            }
            Event::Opened {
                request_id: 3,
                attempt,
            } if restarting => {
                publish(&root.join("recovered.json"), &attempt);
                break;
            }
            Event::Fatal(error) | Event::RequestFailed { message: error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    drop(service);
}

struct OwnedChild {
    child: Child,
    control: Option<PathBuf>,
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // A SIGKILL deliberately bypasses the app's shutdown handler. Clean up
        // only this test's recorded process groups, never a shared CLI daemon.
        if let Some(control) = &self.control
            && let Ok(files) = std::fs::read_dir(control)
        {
            for file in files
                .flatten()
                .filter(|file| file.file_name().to_string_lossy().starts_with("started-"))
            {
                if let Ok(bytes) = std::fs::read(file.path())
                    && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    && let Some(pid) = value["pid"].as_i64()
                {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
        }
    }
}

fn spawn(paths: &AppPaths, restarting: bool, input: &str) -> OwnedChild {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "recovery_child", "--nocapture"])
        .env("DICTIONARY_RECOVERY_TEST_PROFILE", &paths.data)
        .env(
            "DICTIONARY_RECOVERY_TEST_MODE",
            if restarting { "restart" } else { "crash" },
        )
        .env("DICTIONARY_RECOVERY_TEST_INPUT", input)
        .env("DICTIONARY_CLI", fixtures.join("concurrent-cli.py"))
        .env("DICTIONARY_MODEL", "fixture-model")
        .env("DICTIONARY_SKILL_PATH", fixtures.join("activity-skill.md"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    OwnedChild {
        child,
        // A restart submits no requests. Never reuse old fixture PID files for
        // cleanup once the crashed process's owned groups have been terminated.
        control: (!restarting).then(|| paths.workspace.join("fixture-control")),
    }
}

fn wait_for(root: &Path, file: &str, child: &mut Child) -> Attempt {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(bytes) = std::fs::read(root.join(file)) {
            return serde_json::from_slice(&bytes).unwrap();
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before writing {file}"
        );
        assert!(Instant::now() < deadline, "timed out waiting for {file}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn service_restart_after_sigkill_keeps_checkpoint_without_replaying_inference() {
    assert_crash_recovery("crash recovery fixture", false);
}

#[test]
fn service_restart_after_usage_checkpoint_preserves_reported_counters() {
    assert_crash_recovery("crash after usage fixture", true);
}

fn assert_crash_recovery(input: &str, expect_usage: bool) {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::at(root.path().to_owned()).unwrap();
    let mut crashed = spawn(&paths, false, input);
    let checkpoint = wait_for(root.path(), "checkpoint.json", &mut crashed.child);
    assert_eq!(checkpoint.status, AttemptStatus::Running);
    assert!(!checkpoint.answer.is_empty());
    assert!(checkpoint.started_at.is_some());
    assert!(checkpoint.first_answer_ms.is_some());
    assert_eq!(checkpoint.usage.total().is_some(), expect_usage);
    drop(crashed);
    let mut restarted = spawn(&paths, true, input);
    let recovered = wait_for(root.path(), "recovered.json", &mut restarted.child);
    assert!(restarted.child.wait().unwrap().success());
    assert_eq!(recovered.id, checkpoint.id);
    assert_eq!(recovered.lookup_id, checkpoint.lookup_id);
    assert_eq!(recovered.answer, checkpoint.answer);
    assert_eq!(recovered.skill_hash, checkpoint.skill_hash);
    assert_eq!(recovered.started_at, checkpoint.started_at);
    assert_eq!(recovered.first_answer_ms, checkpoint.first_answer_ms);
    assert_eq!(recovered.thread_id, checkpoint.thread_id);
    assert_eq!(recovered.activity, checkpoint.activity);
    assert_eq!(recovered.status, AttemptStatus::Interrupted);
    assert_eq!(recovered.failure.unwrap().kind, "interrupted");
    assert!(
        recovered.duration_ms.is_none(),
        "a crash cannot supply a measured final duration"
    );
    assert_eq!(recovered.usage, checkpoint.usage);
    let db = rusqlite::Connection::open(paths.database).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    let starts = std::fs::read_dir(paths.workspace.join("fixture-control"))
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().starts_with("started-"))
        .count();
    assert_eq!(starts, 1, "restart must not submit another lookup");
}

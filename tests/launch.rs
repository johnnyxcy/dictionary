//! A saved connection must work in a Finder-like environment without a shell.
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use dictionary::{
    direction::DirectionChoice,
    discovery::AppPaths,
    model::{Attempt, AttemptStatus},
    service::{Command as ServiceCommand, Event, Service},
    settings::{Appearance, Preferences},
    store::Database,
};

#[test]
#[ignore = "subprocess entry point with an isolated launch environment"]
fn finder_launch_child() {
    let root = PathBuf::from(std::env::var_os("DICTIONARY_LAUNCH_TEST_PROFILE").unwrap());
    let (service, events) = Service::start(AppPaths::at(root.clone()).unwrap()).unwrap();
    let mut submitted = false;
    while let Ok(event) = events.recv_blocking() {
        match event {
            Event::Settings(state) => {
                if let Some(error) = &state.diagnostic {
                    panic!("{error}");
                }
            }
            Event::Ready(_) if !submitted => {
                submitted = true;
                service
                    .send(ServiceCommand::Lookup {
                        request_id: 1,
                        input: "break the ice\n/breɪk ði aɪs/".into(),
                        direction: DirectionChoice::EnZh,
                    })
                    .unwrap();
            }
            Event::Finished {
                attempt,
                save_error,
            } => {
                assert!(save_error.is_none(), "{save_error:?}");
                assert_eq!(
                    attempt.status,
                    AttemptStatus::Succeeded,
                    "{:?}",
                    attempt.failure
                );
                std::fs::write(
                    root.join("finished.json"),
                    serde_json::to_vec(&attempt).unwrap(),
                )
                .unwrap();
                break;
            }
            Event::Fatal(error)
            | Event::SetupFailed(error)
            | Event::RequestFailed { message: error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    drop(service);
}

#[test]
fn saved_cli_and_adjacent_runtime_work_without_terminal_path_or_overrides() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::at(root.path().join("profile")).unwrap();
    let bin = root.path().join("custom runtime/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let runtime = bin.join("dictionary-fixture-python");
    std::fs::write(&runtime, "#!/bin/sh\nexec /usr/bin/python3 \"$@\"\n").unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity-cli.py");
    let cli = bin.join("cli");
    std::fs::write(&cli, format!(r##"#!/usr/bin/env dictionary-fixture-python
import json, runpy, sys
if "exec" not in sys.argv:
    runpy.run_path({}, run_name="__main__")
else:
    prompt = sys.stdin.read()
    text = json.loads(prompt.rsplit("lookup_input (JSON string): ", 1)[1])
    print(json.dumps({{"type":"item.completed", "item":{{"id":"a", "type":"agent_message", "text":text + "：打破僵局"}}}}), flush=True)
    print(json.dumps({{"type":"turn.completed", "usage":{{"input_tokens":100, "output_tokens":20}}}}), flush=True)
"##, serde_json::to_string(&fixture.to_string_lossy()).unwrap())).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let launcher = root.path().join("chosen-cli");
    symlink(&cli, &launcher).unwrap();
    let preferences = Preferences {
        version: 1,
        model: "fixture-model".into(),
        effort: "low".into(),
        fast_mode: false,
        skill_path: fixture.with_file_name("activity-skill.md"),
        cli_path: Some(launcher.clone()),
        appearance: Appearance::Dark,
    };
    Database::open(&paths.database)
        .unwrap()
        .write_state("preferences", &preferences)
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "finder_launch_child", "--nocapture"])
        .env("DICTIONARY_LAUNCH_TEST_PROFILE", &paths.data)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("DICTIONARY_CLI")
        .env_remove("DICTIONARY_MODEL")
        .env_remove("DICTIONARY_SKILL_PATH")
        .current_dir("/")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("saved CLI launch did not finish");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result: Attempt =
        serde_json::from_slice(&std::fs::read(paths.data.join("finished.json")).unwrap()).unwrap();
    assert_eq!(
        result.setup.executable, launcher,
        "preserve the chosen symlink for argv[0] launchers"
    );
    assert_eq!(result.answer, format!("{}：打破僵局", result.input));
    assert_eq!(result.usage.total(), Some(120));
    let db = Database::open(&paths.database).unwrap();
    assert_eq!(db.history().unwrap().len(), 1);
    assert_eq!(
        db.read_state::<Preferences>("preferences")
            .unwrap()
            .unwrap(),
        preferences
    );
}

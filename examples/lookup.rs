//! Exercise the same service as the desktop app without loading GPUI.
use dictionary::{
    direction::DirectionChoice,
    discovery::AppPaths,
    service::{Command, Event, Service},
};

fn main() -> anyhow::Result<()> {
    let input = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: lookup 'word, phrase, or sentence'"))?;
    let paths = AppPaths::discover()?;
    eprintln!("History: {}", paths.database.display());
    let (service, events) = Service::start(paths)?;
    let mut submitted = false;
    while let Ok(event) = events.recv_blocking() {
        match event {
            Event::Opened { attempt, .. } if !attempt.status.is_active() => {
                println!("{}", attempt.answer);
                eprintln!(
                    "Opened saved attempt {}; no model request was made",
                    attempt.id
                );
                break;
            }
            Event::Ready(setup) if !submitted => {
                submitted = true;
                eprintln!("{} · {} · {}", setup.model, setup.effort, setup.skill_name);
                service.send(Command::Lookup {
                    request_id: 1,
                    input: input.clone(),
                    direction: DirectionChoice::Auto,
                })?;
            }
            Event::Finished {
                attempt,
                save_error,
            } => {
                println!("{}", attempt.answer);
                eprintln!(
                    "Status: {:?}; usage: {:?}; duration: {:?} ms; attempt: {}",
                    attempt.status, attempt.usage, attempt.duration_ms, attempt.id
                );
                if let Some(error) = save_error {
                    anyhow::bail!(error);
                }
                if let Some(error) = attempt.failure {
                    anyhow::bail!(error.message);
                }
                break;
            }
            Event::SetupFailed(error)
            | Event::Fatal(error)
            | Event::Notice(error)
            | Event::RequestFailed { message: error, .. } => {
                anyhow::bail!(error)
            }
            _ => {}
        }
    }
    Ok(())
}

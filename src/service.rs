use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use async_channel::{Receiver, Sender};
use tokio::{
    sync::watch,
    task::{Id, JoinError, JoinSet},
};

use crate::{
    discovery::{self, AppPaths},
    model::{
        Attempt, AttemptStatus, Failure, HistoryEntry, Setup, SkillSnapshot, compose_prompt,
        lookup_key, now, validate_input,
    },
    runner::{self, Progress},
    store::Store,
};

pub const MAX_CONCURRENT_LOOKUPS: usize = 3;
pub type RequestId = u64;

pub enum Command {
    Lookup {
        request_id: RequestId,
        input: String,
    },
    Regenerate {
        request_id: RequestId,
        attempt_id: String,
    },
    Open {
        request_id: RequestId,
        attempt_id: String,
    },
    DeleteLookup(String),
    Cancel(String),
    Reconnect,
}

#[derive(Clone, Debug)]
pub enum Event {
    History(Vec<HistoryEntry>),
    Connecting,
    Ready(Setup),
    SetupFailed(String),
    Opened {
        request_id: RequestId,
        attempt: Box<Attempt>,
    },
    RequestFailed {
        request_id: RequestId,
        message: String,
    },
    Started(Box<Attempt>),
    Progress(Progress),
    DeletedLookup(String),
    DeleteFailed {
        lookup_id: String,
        message: String,
    },
    Finished {
        attempt: Box<Attempt>,
        save_error: Option<String>,
    },
    Notice(String),
    Fatal(String),
}

pub struct Service {
    commands: Sender<Command>,
    events: Sender<Event>,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start(paths: AppPaths) -> Result<(Self, Receiver<Event>)> {
        Self::start_inner(paths, None)
    }

    fn start_inner(paths: AppPaths, setup: Option<Setup>) -> Result<(Self, Receiver<Event>)> {
        let (commands, receiver) = async_channel::bounded(16);
        let (events, output) = async_channel::bounded(64);
        let (shutdown, stopping) = watch::channel(false);
        let worker_events = events.clone();
        let thread = std::thread::Builder::new()
            .name("dictionary-runtime".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = worker_events.send_blocking(Event::Fatal(error.to_string()));
                        return;
                    }
                };
                runtime.block_on(async move {
                    if let Err(error) =
                        work(paths, receiver, worker_events.clone(), stopping, setup).await
                    {
                        let _ = worker_events.send(Event::Fatal(format!("{error:#}"))).await;
                    }
                });
            })?;
        Ok((
            Self {
                commands,
                events,
                shutdown,
                thread: Some(thread),
            },
            output,
        ))
    }

    pub fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| anyhow!("The background worker is busy or unavailable. Please try again."))
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.commands.close();
        // Never let an unconsumed UI event block process cleanup on exit.
        self.events.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Job {
    attempt: Attempt,
    prompt: Option<String>,
    cancellation: Option<watch::Sender<bool>>,
    submitted: Instant,
}

enum LookupRequest {
    Search(String),
    Regenerate(String),
}

/// One coordinator admits requests serially, while independent tasks run the CLI.
/// A key stays reserved from durable admission through completion, including queue time.
struct Coordinator {
    store: Store,
    workspace: PathBuf,
    jobs: HashMap<String, Job>,
    keys: HashMap<String, String>,
    queue: VecDeque<String>,
    tasks: JoinSet<Attempt>,
    task_attempts: HashMap<Id, String>,
    unsaved: HashMap<String, Attempt>,
}

impl Coordinator {
    fn new(store: Store, workspace: PathBuf) -> Self {
        Self {
            store,
            workspace,
            jobs: HashMap::new(),
            keys: HashMap::new(),
            queue: VecDeque::new(),
            tasks: JoinSet::new(),
            task_attempts: HashMap::new(),
            unsaved: HashMap::new(),
        }
    }

    async fn open(&self, id: String) -> Result<Attempt> {
        if let Some(job) = self.jobs.get(&id) {
            return Ok(job.attempt.clone());
        }
        if let Some(attempt) = self.unsaved.get(&id) {
            return Ok(attempt.clone());
        }
        self.store.get(id).await
    }

    async fn accept(&mut self, setup: Option<&Setup>, request: LookupRequest) -> Result<Attempt> {
        let submitted = Instant::now();
        let (input, lookup_id) = match request {
            LookupRequest::Search(input) => {
                validate_input(&input)?;
                (input, None)
            }
            LookupRequest::Regenerate(id) => {
                let original = self.open(id).await?;
                (original.input, Some(original.lookup_id))
            }
        };
        let key = lookup_key(&input);
        if let Some(id) = self.keys.get(&key) {
            return self.open(id.clone()).await;
        }
        if lookup_id.is_none()
            && let Some(cached) = self.store.cached(input.clone()).await?
        {
            return Ok(cached);
        }
        let setup = setup
            .context("Connect before starting a new lookup")?
            .clone();
        let skill = SkillSnapshot::read(&setup)?;
        let prompt = compose_prompt(&input, &skill)?;
        let mut attempt = Attempt::new(input, setup, skill.hash.clone());
        if let Some(id) = lookup_id {
            attempt.lookup_id = id;
        }
        self.store
            .insert(attempt.clone(), skill)
            .await
            .context("Cannot save a queued lookup. No request was sent")?;
        // Admission is awaited in the command loop, never spawned: another click
        // cannot pass the key check while this pending record is being saved.
        self.keys.insert(key, attempt.id.clone());
        self.queue.push_back(attempt.id.clone());
        self.jobs.insert(
            attempt.id.clone(),
            Job {
                attempt: attempt.clone(),
                prompt: Some(prompt),
                cancellation: None,
                submitted,
            },
        );
        Ok(attempt)
    }

    async fn history(&self, events: &Sender<Event>) {
        match self.store.history().await {
            Ok(history) => {
                events.send(Event::History(history)).await.ok();
            }
            Err(error) => {
                events
                    .send(Event::Notice(format!("Cannot refresh history: {error:#}")))
                    .await
                    .ok();
            }
        }
    }

    async fn launch_ready(
        &mut self,
        progress: &Sender<Progress>,
        events: &Sender<Event>,
        stopping: &watch::Receiver<bool>,
    ) {
        while self.tasks.len() < MAX_CONCURRENT_LOOKUPS && !*stopping.borrow() {
            let Some(id) = self.queue.pop_front() else {
                break;
            };
            let Some(job) = self.jobs.get_mut(&id) else {
                continue;
            };
            job.attempt.status = AttemptStatus::Running;
            job.attempt.queue_duration_ms = Some(job.submitted.elapsed().as_millis() as u64);
            let mut attempt = job.attempt.clone();
            let submitted = job.submitted;
            let prompt = job.prompt.take().expect("queued request has a prompt");
            if let Err(error) = self.store.save(attempt.clone()).await {
                attempt.status = AttemptStatus::Failed;
                attempt.completed_at = Some(now());
                attempt.failure = Some(Failure::new(
                    "storage",
                    format!("Cannot start lookup: {error:#}. No request was sent."),
                ));
                self.finish(attempt, events).await;
                continue;
            }
            if *stopping.borrow() {
                break;
            }
            let (cancellation, cancelled) = watch::channel(false);
            self.jobs.get_mut(&id).expect("admitted job").cancellation = Some(cancellation);
            let workspace = self.workspace.clone();
            let progress = progress.clone();
            let initial = attempt.clone();
            let task = self.tasks.spawn(async move {
                runner::run(
                    attempt,
                    prompt,
                    &workspace,
                    cancelled,
                    progress,
                    Duration::from_secs(180),
                    submitted,
                )
                .await
            });
            self.task_attempts.insert(task.id(), id);
            events.send(Event::Started(Box::new(initial))).await.ok();
        }
    }

    async fn update(&mut self, update: Progress, events: &Sender<Event>) {
        if let Some(job) = self.jobs.get_mut(&update.attempt_id) {
            job.attempt.answer = update.answer.clone();
            job.attempt.activity = update.activity.clone();
            events.send(Event::Progress(update)).await.ok();
        }
    }

    async fn finish_task(
        &mut self,
        result: std::result::Result<(Id, Attempt), JoinError>,
        events: &Sender<Event>,
    ) {
        let task_id = match &result {
            Ok((id, _)) => *id,
            Err(error) => error.id(),
        };
        let Some(id) = self.task_attempts.remove(&task_id) else {
            return;
        };
        let attempt = match result {
            Ok((_, attempt)) => attempt,
            Err(error) => {
                let Some(job) = self.jobs.get(&id) else {
                    return;
                };
                let mut attempt = job.attempt.clone();
                attempt.status = AttemptStatus::Failed;
                attempt.completed_at = Some(now());
                attempt.failure = Some(Failure::new(
                    "worker",
                    format!("Lookup worker failed: {error}"),
                ));
                attempt
            }
        };
        self.finish(attempt, events).await;
    }

    async fn finish(&mut self, mut attempt: Attempt, events: &Sender<Event>) {
        let Some(job) = self.jobs.remove(&attempt.id) else {
            return;
        };
        self.keys.remove(&lookup_key(&attempt.input));
        self.queue.retain(|id| id != &attempt.id);
        let save_error = match self.store.finalize(attempt.clone(), job.submitted).await {
            Ok(saved) => {
                attempt = saved;
                self.unsaved.remove(&attempt.id);
                None
            }
            Err(error) => {
                attempt.duration_ms = Some(job.submitted.elapsed().as_millis() as u64);
                self.unsaved.insert(attempt.id.clone(), attempt.clone());
                Some(format!(
                    "Could not finish saving this lookup: {error:#}. Copy the entry before closing."
                ))
            }
        };
        events
            .send(Event::Finished {
                attempt: Box::new(attempt),
                save_error,
            })
            .await
            .ok();
        self.history(events).await;
    }

    async fn cancel(&mut self, id: &str, events: &Sender<Event>) {
        let Some(job) = self.jobs.get(id) else {
            return;
        };
        if let Some(cancellation) = &job.cancellation {
            let _ = cancellation.send(true);
        } else {
            let mut attempt = job.attempt.clone();
            attempt.status = AttemptStatus::Cancelled;
            attempt.completed_at = Some(now());
            attempt.queue_duration_ms = Some(job.submitted.elapsed().as_millis() as u64);
            attempt.failure = Some(Failure::new(
                "cancelled",
                "Queued lookup canceled before it started",
            ));
            self.finish(attempt, events).await;
        }
    }

    async fn shutdown(&mut self, events: &Sender<Event>) {
        // Signal every process first; one slow cleanup must not delay cancellation of others.
        for job in self.jobs.values() {
            if let Some(cancellation) = &job.cancellation {
                let _ = cancellation.send(true);
            }
        }
        let queued: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.cancellation.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        for id in queued {
            self.cancel(&id, events).await;
        }
        while let Some(result) = self.tasks.join_next_with_id().await {
            self.finish_task(result, events).await;
        }
    }
}

async fn work(
    paths: AppPaths,
    commands: Receiver<Command>,
    events: Sender<Event>,
    mut stopping: watch::Receiver<bool>,
    mut setup: Option<Setup>,
) -> Result<()> {
    let store = Store::open(paths.database).await?;
    let mut coordinator = Coordinator::new(store, paths.workspace.clone());
    coordinator.history(&events).await;
    let mut discovery_task = if let Some(setup) = &setup {
        events.send(Event::Ready(setup.clone())).await.ok();
        None
    } else {
        Some(tokio::spawn(discovery::discover(paths.workspace.clone())))
    };
    let (progress, updates) = async_channel::bounded::<Progress>(96);
    loop {
        if *stopping.borrow() {
            break;
        }
        coordinator
            .launch_ready(&progress, &events, &stopping)
            .await;
        tokio::select! {
            biased;
            _ = stopping.changed() => break,
            result = coordinator.tasks.join_next_with_id(), if !coordinator.tasks.is_empty() => {
                if let Some(result) = result { coordinator.finish_task(result, &events).await; }
            }
            command = commands.recv() => {
                let Ok(command) = command else { break; };
                let (request_id, result) = match command {
                    Command::Lookup { request_id, input } =>
                        (request_id, coordinator.accept(setup.as_ref(), LookupRequest::Search(input)).await),
                    Command::Regenerate { request_id, attempt_id } =>
                        (request_id, coordinator.accept(setup.as_ref(), LookupRequest::Regenerate(attempt_id)).await),
                    Command::Open { request_id, attempt_id } =>
                        (request_id, coordinator.open(attempt_id).await),
                    Command::Cancel(id) => { coordinator.cancel(&id, &events).await; continue; }
                    Command::DeleteLookup(lookup_id) => {
                        match coordinator.store.delete_lookup(lookup_id.clone()).await {
                            Ok(()) => {
                                coordinator.unsaved.retain(|_, attempt| attempt.lookup_id != lookup_id);
                                events.send(Event::DeletedLookup(lookup_id)).await.ok();
                                coordinator.history(&events).await;
                            }
                            Err(error) => {
                                events.send(Event::DeleteFailed { lookup_id, message: format!("Cannot delete lookup: {error:#}") }).await.ok();
                            }
                        }
                        continue;
                    }
                    Command::Reconnect => {
                        if coordinator.jobs.is_empty() && discovery_task.is_none() {
                            setup = None;
                            events.send(Event::Connecting).await.ok();
                            discovery_task = Some(tokio::spawn(discovery::discover(paths.workspace.clone())));
                        }
                        continue;
                    }
                };
                match result {
                    Ok(attempt) => { events.send(Event::Opened { request_id, attempt: Box::new(attempt) }).await.ok(); }
                    Err(error) => { events.send(Event::RequestFailed { request_id, message: format!("Cannot open lookup: {error:#}") }).await.ok(); }
                }
            }
            update = updates.recv() => {
                if let Ok(update) = update { coordinator.update(update, &events).await; }
            }
            result = async { discovery_task.as_mut().expect("guarded metadata task").await }, if discovery_task.is_some() => {
                discovery_task = None;
                match result {
                    Ok(Ok(value)) => { setup = Some(value.clone()); events.send(Event::Ready(value)).await.ok(); }
                    Ok(Err(error)) => { events.send(Event::SetupFailed(format!("{error:#}"))).await.ok(); }
                    Err(error) => { events.send(Event::SetupFailed(format!("Setup worker failed: {error}"))).await.ok(); }
                }
            }
        }
    }
    if let Some(task) = discovery_task {
        task.abort();
        let _ = task.await;
    }
    events.close();
    coordinator.shutdown(&events).await;
    Ok(())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

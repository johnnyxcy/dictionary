use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
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
    analysis::{AnalysisQuery, AnalysisReport},
    direction::{Direction, DirectionChoice},
    discovery::{self, AppPaths},
    history::{HistoryPage, HistoryQuery},
    model::{
        Attempt, AttemptStatus, Failure, HistoryEntry, Setup, SkillSnapshot, compose_prompt,
        lookup_key, now, validate_input,
    },
    runner::{self, Progress},
    settings::{Catalog, Preferences, SettingsState},
    store::Store,
};

pub const MAX_CONCURRENT_LOOKUPS: usize = 3;
pub type RequestId = u64;

pub enum Command {
    QueryAnalysis {
        request_id: RequestId,
        query: AnalysisQuery,
    },
    QueryHistory {
        request_id: RequestId,
        query: HistoryQuery,
    },
    Lookup {
        request_id: RequestId,
        input: String,
        direction: DirectionChoice,
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
    SaveSettings(Preferences),
}

#[derive(Clone, Debug)]
pub enum Event {
    Analysis {
        request_id: RequestId,
        report: Arc<AnalysisReport>,
    },
    AnalysisFailed {
        request_id: RequestId,
        message: String,
    },
    History(Vec<HistoryEntry>),
    HistoryPage {
        request_id: RequestId,
        page: HistoryPage,
    },
    HistoryFailed {
        request_id: RequestId,
        message: String,
    },
    HistoryChanged,
    Connecting,
    Ready(Setup),
    Settings(Box<SettingsState>),
    SettingsSaved,
    SettingsFailed(String),
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

    fn start_inner(paths: AppPaths, catalog: Option<Catalog>) -> Result<(Self, Receiver<Event>)> {
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
                        work(paths, receiver, worker_events.clone(), stopping, catalog).await
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
    checkpoint_failed: bool,
}

enum LookupRequest {
    Search(String, DirectionChoice),
    Regenerate(String),
}

/// One coordinator admits requests serially, while independent tasks run the CLI.
/// A key stays reserved from durable admission through completion, including queue time.
struct Coordinator {
    store: Store,
    workspace: PathBuf,
    jobs: HashMap<String, Job>,
    keys: HashMap<(Direction, String), String>,
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
        let (input, lookup_id, requested_direction, direction) = match request {
            LookupRequest::Search(input, requested) => {
                validate_input(&input)?;
                let direction = requested.resolve(&input)?;
                (input, None, requested, direction)
            }
            LookupRequest::Regenerate(id) => {
                let original = self.open(id).await?;
                (
                    original.input,
                    Some(original.lookup_id),
                    original.requested_direction,
                    original.direction,
                )
            }
        };
        let key = (direction, lookup_key(&input));
        if let Some(id) = self.keys.get(&key) {
            return self.open(id.clone()).await;
        }
        if lookup_id.is_none()
            && let Some(cached) = self.store.cached(input.clone(), direction).await?
        {
            return Ok(cached);
        }
        let setup = setup
            .context("Connect before starting a new lookup")?
            .clone();
        let skill = SkillSnapshot::read(&setup)?;
        let prompt = compose_prompt(&input, &skill, direction)?;
        let mut attempt = Attempt::new(input, setup, skill.hash.clone());
        attempt.direction = direction;
        attempt.requested_direction = requested_direction;
        attempt.prompt_version = direction.prompt_version().into();
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
                checkpoint_failed: false,
            },
        );
        Ok(attempt)
    }

    async fn history(&self, events: &Sender<Event>) {
        events.send(Event::HistoryChanged).await.ok();
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
            events.send(Event::HistoryChanged).await.ok();
        }
    }

    async fn update(&mut self, update: Progress, events: &Sender<Event>) {
        if let Some(job) = self.jobs.get_mut(&update.attempt_id) {
            update.apply_to(&mut job.attempt);
            // Checkpoint real output before announcing it. Restart recovery can
            // retain the latest saved partial result without replaying inference.
            match self.store.save(job.attempt.clone()).await {
                Ok(()) => job.checkpoint_failed = false,
                Err(error) => {
                    if !job.checkpoint_failed {
                        events.send(Event::Notice(format!("Cannot save live progress: {error:#}. Keep Dictionary open until the result is saved, or copy it before closing."))).await.ok();
                    }
                    job.checkpoint_failed = true;
                }
            }
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
        self.keys
            .remove(&(attempt.direction, lookup_key(&attempt.input)));
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
    catalog: Option<Catalog>,
) -> Result<()> {
    let store = Store::open(paths.database).await?;
    let mut settings = SettingsState {
        preferences: store.preferences().await?,
        catalog: store.catalog().await?,
        cached: true,
        diagnostic: None,
    };
    let mut coordinator = Coordinator::new(store, paths.workspace.clone());
    coordinator.history(&events).await;
    let mut discovery_task = if let Some(catalog) = catalog {
        install_catalog(&mut settings, catalog, &coordinator.store).await?;
        None
    } else {
        Some(tokio::spawn(discovery::discover_catalog(
            paths.workspace.clone(),
        )))
    };
    let mut setup = if settings.catalog.is_some() {
        publish_settings(&settings, &events).await
    } else {
        None
    };
    if discovery_task.is_some() {
        events.send(Event::Connecting).await.ok();
    }
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
                    Command::QueryAnalysis { request_id, query } => {
                        match coordinator.store.analysis(query).await {
                            Ok(report) => { events.send(Event::Analysis { request_id, report: Arc::new(report) }).await.ok(); }
                            Err(error) => { events.send(Event::AnalysisFailed { request_id, message: format!("Cannot load analysis: {error:#}") }).await.ok(); }
                        }
                        continue;
                    }
                    Command::QueryHistory { request_id, query } => {
                        match coordinator.store.history_page(query).await {
                            Ok(page) => { events.send(Event::HistoryPage { request_id, page }).await.ok(); }
                            Err(error) => { events.send(Event::HistoryFailed { request_id, message: format!("Cannot load history: {error:#}") }).await.ok(); }
                        }
                        continue;
                    }
                    Command::Lookup { request_id, input, direction } =>
                        (request_id, coordinator.accept(setup.as_ref(), LookupRequest::Search(input, direction)).await),
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
                        if discovery_task.is_none() {
                            events.send(Event::Connecting).await.ok();
                            discovery_task = Some(tokio::spawn(discovery::discover_catalog(paths.workspace.clone())));
                        }
                        continue;
                    }
                    Command::SaveSettings(preferences) => {
                        let result = async {
                            let catalog = settings.catalog.as_ref().context("Refresh the model catalog before saving Settings")?;
                            let chosen = preferences.setup(catalog)?;
                            SkillSnapshot::read(&chosen).context("Cannot read the chosen dictionary skill")?;
                            coordinator.store.save_preferences(preferences.clone()).await?;
                            Ok::<_, anyhow::Error>(chosen)
                        }.await;
                        match result {
                            Ok(chosen) => {
                                settings.preferences = Some(preferences);
                                if !settings.cached { settings.diagnostic = None; }
                                setup = Some(chosen.clone());
                                events.send(Event::Ready(chosen)).await.ok();
                                events.send(Event::Settings(Box::new(settings.clone()))).await.ok();
                                events.send(Event::SettingsSaved).await.ok();
                            }
                            Err(error) => { events.send(Event::SettingsFailed(format!("{error:#}"))).await.ok(); }
                        }
                        continue;
                    }
                };
                match result {
                    Ok(attempt) => {
                        let active = attempt.status.is_active();
                        events.send(Event::Opened { request_id, attempt: Box::new(attempt) }).await.ok();
                        if active { events.send(Event::HistoryChanged).await.ok(); }
                    }
                    Err(error) => { events.send(Event::RequestFailed { request_id, message: format!("Cannot open lookup: {error:#}") }).await.ok(); }
                }
            }
            update = updates.recv() => {
                if let Ok(update) = update { coordinator.update(update, &events).await; }
            }
            result = async { discovery_task.as_mut().expect("guarded metadata task").await }, if discovery_task.is_some() => {
                discovery_task = None;
                let result = match result {
                    Ok(Ok(catalog)) => install_catalog(&mut settings, catalog, &coordinator.store).await,
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(anyhow!("Setup worker failed: {error}")),
                };
                if let Err(error) = result {
                    settings.cached = true;
                    settings.diagnostic = Some(format!("{error:#}"));
                }
                setup = publish_settings(&settings, &events).await;
                if settings.catalog.is_none() {
                    events.send(Event::SetupFailed(settings.diagnostic.clone().unwrap_or_else(|| "Model catalog is unavailable".into()))).await.ok();
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

async fn install_catalog(
    settings: &mut SettingsState,
    catalog: Catalog,
    store: &Store,
) -> Result<()> {
    store.save_catalog(catalog.clone()).await?;
    if settings.preferences.is_none() {
        let preferences = Preferences::initial(&catalog);
        store.save_preferences(preferences.clone()).await?;
        settings.preferences = Some(preferences);
    }
    settings.diagnostic = catalog.skill_diagnostic.clone();
    settings.catalog = Some(catalog);
    settings.cached = false;
    Ok(())
}

async fn publish_settings(settings: &SettingsState, events: &Sender<Event>) -> Option<Setup> {
    events
        .send(Event::Settings(Box::new(settings.clone())))
        .await
        .ok();
    let result = settings.setup().and_then(|setup| {
        SkillSnapshot::read(&setup)
            .context("Cannot read the dictionary skill. Choose its current location in Settings")?;
        Ok(setup)
    });
    match result {
        Ok(setup) => {
            events.send(Event::Ready(setup.clone())).await.ok();
            Some(setup)
        }
        Err(error) => {
            events
                .send(Event::SetupFailed(format!("{error:#}")))
                .await
                .ok();
            None
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

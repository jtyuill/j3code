//! Connection-level state: status, descriptor, server config and the shell
//! (projects + active threads), reduced like t3's client-runtime
//! `applyShellStreamEvent` (`packages/client-runtime/src/state/shellReducer.ts`)
//! and `applyServerConfigProjection` (`serverConfigProjection.ts`).

use std::collections::BTreeMap;

use j3_protocol::t3::{
    environment::ExecutionEnvironmentDescriptor,
    orchestration::{
        ActivityRunStatus, RuntimeRequestKind, ShellLocation, ShellStreamItem, ShellThreadStatus,
        ThreadRelationship, ThreadShell,
    },
    project::ProjectShell,
    server::{
        ServerConfig, ServerConfigStreamEvent, ServerProviderSkill, ServerProviderSlashCommand,
    },
};

/// Everything known about one server.
#[derive(Debug, Clone, Default)]
pub struct ConnectionState {
    pub status: ConnectionStatus,
    /// From `GET /.well-known/t3/environment`, kept across reconnects.
    pub descriptor: Option<ExecutionEnvironmentDescriptor>,
    /// From `subscribeServerConfig`: providers (models, thinking options,
    /// slash commands, skills), settings, environment.
    pub config: Option<ServerConfig>,
    pub shell: ShellState,
    /// Workspace slash commands and skills fetched by
    /// [`crate::Connection::refresh_workspace`], by cwd.
    pub workspaces: BTreeMap<String, WorkspaceCommands>,
}

/// Where the connection supervisor is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// First attempt in progress.
    #[default]
    Connecting,
    /// Connected; the shell is loading.
    Synchronizing,
    /// Connected and the shell is up to date.
    Live,
    /// Disconnected; the next attempt starts after a backoff.
    Reconnecting { attempt: u32, error: String },
    /// [`crate::Connection::close`] was called.
    Closed,
}

impl ConnectionStatus {
    /// Calls can be made (`Synchronizing` or `Live`).
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Synchronizing | Self::Live)
    }
}

/// Slash commands and skills available in one workspace directory
/// (`server.refreshProviders` with a `cwd`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceCommands {
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    pub skills: Vec<ServerProviderSkill>,
}

/// The shell: projects and active (unarchived) threads.
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    /// A snapshot has arrived at least once.
    pub loaded: bool,
    /// The `synchronized` marker arrived after the latest (re)subscription.
    pub synchronized: bool,
    pub snapshot_sequence: u64,
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    /// Archived rows carried by snapshots (j3 always sends none; see
    /// [`crate::Connection::archived`]).
    pub archived_threads: Vec<ThreadShell>,
}

impl ShellState {
    pub fn project(&self, project_id: &str) -> Option<&ProjectShell> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    pub fn thread(&self, thread_id: &str) -> Option<&ThreadShell> {
        self.threads.iter().find(|thread| thread.id == thread_id)
    }

    /// The project's threads for a list, newest first (by latest user message,
    /// else creation). Subagent threads are hidden, as in t3's lists.
    pub fn threads_of(&self, project_id: &str) -> Vec<&ThreadShell> {
        let mut threads: Vec<&ThreadShell> = self
            .threads
            .iter()
            .filter(|thread| {
                thread.project_id == project_id
                    && thread.lineage.relationship_to_parent != Some(ThreadRelationship::Subagent)
                    && thread.deleted_at.is_none()
            })
            .collect();
        threads.sort_by(|a, b| {
            let key = |thread: &ThreadShell| {
                thread
                    .latest_user_message_at
                    .clone()
                    .unwrap_or_else(|| thread.created_at.clone())
            };
            key(b).cmp(&key(a)).then_with(|| b.id.cmp(&a.id))
        });
        threads
    }

    /// Applies one `orchestration.subscribeShell` item. Returns whether
    /// anything changed.
    pub(crate) fn apply(&mut self, item: ShellStreamItem) -> bool {
        match item {
            ShellStreamItem::Snapshot(item) => {
                let snapshot = item.snapshot;
                self.loaded = true;
                self.snapshot_sequence = snapshot.snapshot_sequence;
                self.projects = snapshot.projects;
                self.threads = snapshot.threads;
                self.archived_threads = snapshot.archived_threads;
                true
            }
            ShellStreamItem::Synchronized => !std::mem::replace(&mut self.synchronized, true),
            ShellStreamItem::ProjectUpdated(update) => {
                if update.sequence <= self.snapshot_sequence {
                    return false;
                }
                self.snapshot_sequence = update.sequence;
                let mut project = update.project;
                match self.projects.iter().position(|p| p.id == project.id) {
                    Some(index) => {
                        // `retainRepositoryIdentity`.
                        let previous = &self.projects[index];
                        if project.repository_identity.clone().flatten().is_none()
                            && previous.repository_identity.clone().flatten().is_some()
                            && previous.workspace_root == project.workspace_root
                        {
                            project.repository_identity = previous.repository_identity.clone();
                        }
                        self.projects[index] = project;
                    }
                    None => self.projects.push(project),
                }
                true
            }
            ShellStreamItem::ProjectRemoved(removed) => {
                if removed.sequence <= self.snapshot_sequence {
                    return false;
                }
                self.snapshot_sequence = removed.sequence;
                self.projects.retain(|p| p.id != removed.project_id);
                true
            }
            ShellStreamItem::ThreadUpdated(update) => {
                if update.sequence <= self.snapshot_sequence {
                    return false;
                }
                self.snapshot_sequence = update.sequence;
                let thread = update.thread;
                self.archived_threads.retain(|t| t.id != thread.id);
                match update.location {
                    ShellLocation::Active => {
                        match self.threads.iter().position(|t| t.id == thread.id) {
                            Some(index) => self.threads[index] = thread,
                            None => self.threads.push(thread),
                        }
                    }
                    ShellLocation::Archive => self.threads.retain(|t| t.id != thread.id),
                }
                true
            }
            ShellStreamItem::ThreadRemoved(removed) => {
                if removed.sequence <= self.snapshot_sequence {
                    return false;
                }
                self.snapshot_sequence = removed.sequence;
                self.threads.retain(|t| t.id != removed.thread_id);
                self.archived_threads.retain(|t| t.id != removed.thread_id);
                true
            }
            ShellStreamItem::Unknown(_) => false,
        }
    }
}

/// Applies one `subscribeServerConfig` item after the handshake snapshot.
pub(crate) fn apply_config(config: &mut Option<ServerConfig>, event: ServerConfigStreamEvent) {
    match event {
        ServerConfigStreamEvent::Snapshot(snapshot) => *config = Some(snapshot.config),
        ServerConfigStreamEvent::ProviderStatuses(update) => {
            if let Some(config) = config {
                config.providers = update.payload.providers;
            }
        }
        ServerConfigStreamEvent::SettingsUpdated(update) => {
            if let Some(config) = config {
                config.settings = update.payload.settings;
            }
        }
        ServerConfigStreamEvent::KeybindingsUpdated(update) => {
            if let Some(config) = config {
                config.keybindings = update.payload.keybindings;
                config.issues = update.payload.issues;
            }
        }
        ServerConfigStreamEvent::EnvironmentThemesUpdated(_)
        | ServerConfigStreamEvent::UsageLimitSourcesUpdated(_)
        | ServerConfigStreamEvent::Unknown(_) => {}
    }
}

/// A thread row's state for a list badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadActivity {
    Running,
    WaitingForInput,
    Error,
    Idle,
}

impl ThreadActivity {
    /// Like t3's list model: a pending `user_input` request waits for input;
    /// otherwise `activityRunStatus ?? status` decides.
    pub fn of(thread: &ThreadShell) -> Self {
        if thread
            .pending_runtime_request
            .as_ref()
            .is_some_and(|request| request.kind == RuntimeRequestKind::UserInput)
        {
            return Self::WaitingForInput;
        }
        if let Some(Some(
            ActivityRunStatus::Preparing
            | ActivityRunStatus::Starting
            | ActivityRunStatus::Running
            | ActivityRunStatus::Waiting,
        )) = thread.activity_run_status
        {
            return Self::Running;
        }
        match thread.status {
            ShellThreadStatus::Preparing
            | ShellThreadStatus::Queued
            | ShellThreadStatus::Starting
            | ShellThreadStatus::Running
            | ShellThreadStatus::Waiting => Self::Running,
            ShellThreadStatus::Failed => Self::Error,
            ShellThreadStatus::Idle
            | ShellThreadStatus::Completed
            | ShellThreadStatus::Interrupted
            | ShellThreadStatus::Cancelled
            | ShellThreadStatus::RolledBack => Self::Idle,
        }
    }
}

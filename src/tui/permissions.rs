use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::os::unix::net::UnixStream;

use crate::primitives::{ProjectId, TaskName};
use crate::project::Project;

pub struct ActivePermission {
    pub perm_id: u64,
    pub stream: UnixStream,
    pub task_name: TaskName,
    pub tool_name: String,
    pub tool_input_summary: String,
    pub permission_suggestions: Vec<serde_json::Value>,
    /// AskUser question text (set when tool_name == "AskUserQuestion").
    pub askuser_question: Option<String>,
    /// AskUser options (label, description pairs).
    pub askuser_options: Vec<(String, String)>,
    /// When true, the stream expects a PreToolUse response format
    /// (`{"decision":"allow/deny"}`) instead of PermissionRequest format.
    pub is_pretool: bool,
}

impl ActivePermission {
    pub fn is_askuser(&self) -> bool {
        self.askuser_question.is_some()
    }
}

/// Manages pending permission requests keyed by task name.
pub struct PermissionStore {
    inner: HashMap<TaskName, VecDeque<ActivePermission>>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn add(&mut self, perm: ActivePermission) {
        self.inner
            .entry(perm.task_name.clone())
            .or_default()
            .push_back(perm);
    }

    pub fn take(&mut self, name: &TaskName) -> Option<ActivePermission> {
        let queue = self.inner.get_mut(name)?;
        let perm = queue.pop_front();
        if queue.is_empty() {
            self.inner.remove(name);
        }
        perm
    }

    pub fn peek(&self, name: &TaskName) -> Option<&ActivePermission> {
        self.inner.get(name)?.front()
    }

    /// Sorted list of task names that have pending permissions.
    pub fn task_names_with_pending(&self) -> Vec<TaskName> {
        let mut names: Vec<TaskName> = self.inner.keys().cloned().collect();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        names
    }

    /// Remove and return all permissions for task names that don't correspond
    /// to any globally running task. The "exo" key is always preserved.
    pub fn drain_stale(&mut self, all_running_names: &HashSet<TaskName>) -> Vec<ActivePermission> {
        let stale_keys: Vec<TaskName> = self
            .inner
            .keys()
            .filter(|k| k.as_str() != "exo" && !all_running_names.contains(*k))
            .cloned()
            .collect();

        let mut stale = Vec::new();
        for key in stale_keys {
            if let Some(queue) = self.inner.remove(&key) {
                stale.extend(queue);
            }
        }
        stale
    }

    /// Drain all permissions (used on shutdown).
    pub fn drain_all(
        &mut self,
    ) -> impl Iterator<Item = (TaskName, VecDeque<ActivePermission>)> + '_ {
        self.inner.drain()
    }

    /// All perm IDs currently tracked.
    pub fn all_perm_ids(&self) -> HashSet<u64> {
        self.inner
            .values()
            .flat_map(|q| q.iter().map(|p| p.perm_id))
            .collect()
    }

    /// Count pending permissions only for tasks in the given project.
    /// "exo" key belongs to the default (no-project) scope.
    pub fn count_for_project(
        &self,
        current_pid: Option<&ProjectId>,
        global_task_projects: &HashMap<TaskName, Option<ProjectId>>,
    ) -> usize {
        self.inner
            .iter()
            .filter(|(task_name, _)| is_in_project(task_name, current_pid, global_task_projects))
            .map(|(_, queue)| queue.len())
            .sum()
    }

    /// Count pending AskUser permissions in the given project.
    pub fn askuser_count_for_project(
        &self,
        current_pid: Option<&ProjectId>,
        global_task_projects: &HashMap<TaskName, Option<ProjectId>>,
    ) -> usize {
        self.inner
            .iter()
            .filter(|(task_name, _)| is_in_project(task_name, current_pid, global_task_projects))
            .map(|(_, queue)| queue.iter().filter(|p| p.is_askuser()).count())
            .sum()
    }

    /// Count pending permissions for tasks NOT in the current project.
    /// Returns a vec of (project_name_or_default, count) for display.
    pub fn other_project_counts(
        &self,
        current_pid: Option<&ProjectId>,
        global_task_projects: &HashMap<TaskName, Option<ProjectId>>,
        projects: &[Project],
    ) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (task_name, queue) in &self.inner {
            let task_pid = global_task_projects
                .get(task_name)
                .and_then(|pid| pid.as_ref());
            if task_pid != current_pid {
                let label = if let Some(pid) = task_pid {
                    projects
                        .iter()
                        .find(|p| p.id == *pid)
                        .map(|p| p.name.as_str().to_string())
                        .unwrap_or_else(|| "?".to_string())
                } else {
                    "default".to_string()
                };
                *counts.entry(label).or_default() += queue.len();
            }
        }
        counts.into_iter().collect()
    }

    /// Direct access to the inner map (for iteration in Telegram handlers, widgets, etc.)
    pub fn get(&self, name: &TaskName) -> Option<&VecDeque<ActivePermission>> {
        self.inner.get(name)
    }

    /// Iterate over all task→queue pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&TaskName, &VecDeque<ActivePermission>)> {
        self.inner.iter()
    }
}

/// Helper: does this task name belong to the given project?
fn is_in_project(
    task_name: &TaskName,
    current_pid: Option<&ProjectId>,
    global_task_projects: &HashMap<TaskName, Option<ProjectId>>,
) -> bool {
    if task_name.as_str() == "exo" {
        current_pid.is_none()
    } else {
        let task_pid = global_task_projects
            .get(task_name)
            .and_then(|pid| pid.as_ref());
        task_pid == current_pid
    }
}

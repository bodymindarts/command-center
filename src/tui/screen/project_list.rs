use ratatui::widgets::ListState;

use crate::primitives::{ProjectId, ProjectName, TaskName};
use crate::task::Project;

/// Saved UI state for a project, restored on Ctrl+R.
pub struct SavedProjectState {
    pub name: ProjectName,
    pub id: ProjectId,
    pub show_detail: bool,
    pub selected_task_name: Option<TaskName>,
}

pub struct ProjectListState {
    /// Cached list of projects for rendering.
    pub projects: Vec<Project>,
    /// Selection state for the project list.
    pub list_state: ListState,
    /// Whether the right panel shows the project list instead of the task list.
    pub show_projects: bool,
    /// Currently active project name (for display). None = default (ExO).
    pub active_project: Option<ProjectName>,
    /// Currently active project ID (for queries). None = default (ExO).
    pub active_project_id: Option<ProjectId>,
    /// Last active project state — remembered when Ctrl+O leaves a project.
    pub last_project: Option<SavedProjectState>,
    /// Indices into `projects` that match the current search query.
    pub filtered_project_indices: Vec<usize>,
}

impl ProjectListState {
    pub(super) fn new() -> Self {
        Self {
            projects: Vec::new(),
            list_state: ListState::default(),
            show_projects: false,
            active_project: None,
            active_project_id: None,
            last_project: None,
            filtered_project_indices: Vec::new(),
        }
    }

    pub fn next_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.projects.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.projects.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.list_state
            .selected()
            .and_then(|i| self.projects.get(i))
    }

    /// Refresh the project list and clamp the selection after changes (e.g. delete).
    pub fn refresh_projects(&mut self, projects: Vec<Project>) {
        self.projects = projects;
        if self.projects.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.projects.len().saturating_sub(1));
            self.list_state.select(Some(sel));
        }
    }

    pub fn selected_filtered_project_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_project_indices.get(i).copied())
    }

    pub fn search_next_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.filtered_project_indices.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn search_prev_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_project_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

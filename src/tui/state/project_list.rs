use ratatui::widgets::ListState;

use crate::task::Project;

pub struct ProjectListState {
    /// Cached list of projects for rendering.
    projects: Vec<Project>,
    /// Selection state for the project list.
    pub list_state: ListState,
    /// Whether the right panel shows the project list instead of the task list.
    show_projects: bool,
    /// Indices into `projects` that match the current search query.
    filtered_project_indices: Vec<usize>,
}

impl ProjectListState {
    pub(super) fn new() -> Self {
        Self {
            projects: Vec::new(),
            list_state: ListState::default(),
            show_projects: false,
            filtered_project_indices: Vec::new(),
        }
    }

    // ── Visibility ───────────────────────────────────────────────────

    pub fn show(&mut self, projects: Vec<Project>) {
        self.projects = projects;
        self.show_projects = true;
        if !self.projects.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    pub fn hide(&mut self) {
        self.show_projects = false;
    }

    pub fn is_visible(&self) -> bool {
        self.show_projects
    }

    // ── Projects ─────────────────────────────────────────────────────

    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.projects = projects;
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    // ── Filtered indices ─────────────────────────────────────────────

    pub fn clear_filter(&mut self) {
        self.filtered_project_indices.clear();
    }

    #[cfg(test)]
    pub fn set_filtered_indices(&mut self, indices: Vec<usize>) {
        self.filtered_project_indices = indices;
    }

    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered_project_indices
    }

    // ── Navigation ───────────────────────────────────────────────────

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

    /// Fuzzy-filter projects by name and clamp the selection.
    pub fn filter(&mut self, query: &[char]) {
        let indices: Vec<usize> = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                if query.is_empty() {
                    return true;
                }
                let name = p.name.as_str().to_lowercase();
                let mut qi = 0;
                for c in name.chars() {
                    if c == query[qi] {
                        qi += 1;
                        if qi == query.len() {
                            return true;
                        }
                    }
                }
                false
            })
            .map(|(i, _)| i)
            .collect();
        self.filtered_project_indices = indices;
        if self.filtered_project_indices.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self.list_state.selected().unwrap_or(0);
            if let Some(pos) = self.filtered_project_indices.iter().position(|&i| i == sel) {
                self.list_state.select(Some(pos));
            } else {
                self.list_state.select(Some(0));
            }
        }
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

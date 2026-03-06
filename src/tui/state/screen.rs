use std::collections::HashMap;

use crate::primitives::{ProjectId, ProjectName, TaskName};
use crate::task::{Project, Task};

use super::project_list::ProjectListState;
use super::project_state::ProjectState;
use super::{Focus, InputState, PermissionStore};

pub struct ScreenState {
    /// ExO workspace — always present.
    pub exo: ProjectState,
    /// Per-project workspaces, keyed by project ID.
    pub projects: HashMap<ProjectId, ProjectState>,
    /// Currently active project ID. None = ExO.
    pub active_project_id: Option<ProjectId>,
    /// Currently active project name (for display). None = ExO.
    pub active_project_name: Option<ProjectName>,
    /// Last active project ID — remembered for Ctrl+R restore.
    pub last_project_id: Option<ProjectId>,

    pub project_list: ProjectListState,
    pub should_quit: bool,
    pub focus: Focus,
    pub permissions: PermissionStore,
    /// Transient error message shown in the prompt bar. Cleared on next keypress.
    pub status_error: Option<String>,
    /// Input state for the task search filter.
    pub search_input: InputState,
    /// Global map of task_name → project_id for all running tasks.
    /// Updated every tick from the full (unscoped) active task list.
    pub global_task_projects: HashMap<TaskName, Option<ProjectId>>,
    /// Global list of (task_name, work_dir) for all running tasks.
    /// Used for CWD→task matching in permission/resolved/idle handlers
    /// so lookups work regardless of which project is currently displayed.
    pub global_task_work_dirs: Vec<(TaskName, String)>,
}

impl ScreenState {
    pub fn new(exo: ProjectState) -> Self {
        Self {
            exo,
            projects: HashMap::new(),
            active_project_id: None,
            active_project_name: None,
            last_project_id: None,
            project_list: ProjectListState::new(),
            should_quit: false,
            focus: Focus::ChatInput,
            permissions: PermissionStore::new(),
            status_error: None,
            search_input: InputState::new(),
            global_task_projects: HashMap::new(),
            global_task_work_dirs: Vec::new(),
        }
    }

    /// Add a project workspace.
    pub fn add_project(&mut self, project_id: ProjectId, project_state: ProjectState) {
        self.projects.insert(project_id, project_state);
    }

    pub fn render_loop_starting(&mut self) {
        self.exo.reset_tasks_to_idle();
        for project in self.projects.values_mut() {
            project.reset_tasks_to_idle();
        }
    }

    // ── Active state accessors ───────────────────────────────────────

    /// Get a reference to the active project state (ExO or current project).
    pub fn active_state(&self) -> &ProjectState {
        match &self.active_project_id {
            Some(pid) => self.projects.get(pid).unwrap_or(&self.exo),
            None => &self.exo,
        }
    }

    /// Get a mutable reference to the active project state.
    pub fn active_state_mut(&mut self) -> &mut ProjectState {
        match &self.active_project_id {
            Some(pid) => {
                let pid = pid.clone();
                self.projects.get_mut(&pid).unwrap_or(&mut self.exo)
            }
            None => &mut self.exo,
        }
    }

    // ── Delegates to active TaskListState ─────────────────────────────

    pub fn selected_task(&self) -> Option<&Task> {
        self.active_state().task_list.selected_task()
    }

    pub fn next(&mut self) {
        self.active_state_mut().task_list.next();
    }

    pub fn previous(&mut self) {
        self.active_state_mut().task_list.previous();
    }

    pub fn refresh_tasks(&mut self, tasks: Vec<Task>) {
        self.active_state_mut().task_list.refresh_tasks(tasks);
    }

    pub fn scroll_detail_down(&mut self) {
        self.active_state_mut().task_list.scroll_detail_down();
    }

    pub fn scroll_detail_up(&mut self) {
        self.active_state_mut().task_list.scroll_detail_up();
    }

    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.active_state().task_list.selected_filtered_task_index()
    }

    // ── Delegates to ProjectListState ────────────────────────────────

    pub fn selected_project(&self) -> Option<&Project> {
        self.project_list.selected_project()
    }

    pub fn refresh_projects(&mut self, projects: Vec<Project>) {
        self.project_list.refresh_projects(projects);
    }

    pub fn selected_filtered_project_index(&self) -> Option<usize> {
        self.project_list.selected_filtered_project_index()
    }

    // ── Focus ────────────────────────────────────────────────────────

    pub fn current_focus(&self) -> &Focus {
        &self.focus
    }

    pub fn navigate_focus_down(&mut self) {
        self.focus = Focus::ChatInput;
    }

    pub fn navigate_focus_right(&mut self) {
        self.focus = Focus::TaskList;
    }

    // ── Delegates to active ChatViewState ────────────────────────────

    pub fn update_chat_viewport_height(&mut self, area_height: u16) {
        self.active_state_mut()
            .chat_view
            .update_chat_viewport_height(area_height);
    }

    pub fn scroll_chat_up(&mut self) {
        self.active_state_mut().chat_view.scroll_chat_up();
    }

    pub fn scroll_chat_down(&mut self) {
        self.active_state_mut().chat_view.scroll_chat_down();
    }

    // ── Permissions ──────────────────────────────────────────────────

    /// Count pending permissions only for tasks in the current project.
    pub fn current_project_perm_count(&self) -> usize {
        self.permissions
            .count_for_project(self.active_project_id.as_ref(), &self.global_task_projects)
    }

    /// Count pending permissions for tasks NOT in the current project.
    pub fn other_project_perm_counts(&self) -> Vec<(String, usize)> {
        self.permissions.other_project_counts(
            self.active_project_id.as_ref(),
            &self.global_task_projects,
            &self.project_list.projects,
        )
    }

    /// Count pending AskUser permissions in the current project.
    pub fn current_project_askuser_count(&self) -> usize {
        self.permissions
            .askuser_count_for_project(self.active_project_id.as_ref(), &self.global_task_projects)
    }

    /// Returns the permission key for the currently visible pane.
    /// Task name if viewing a task's detail, "exo" otherwise.
    pub fn focused_perm_key(&self) -> TaskName {
        let active = self.active_state();
        if active.task_list.show_detail {
            active
                .task_list
                .selected_task()
                .map(|t| t.name.clone())
                .unwrap_or_else(|| TaskName::from("exo".to_string()))
        } else {
            TaskName::from("exo".to_string())
        }
    }

    /// Returns the permission key to display in the overlay and act on
    /// with global keybindings. Prefers the focused task's key; falls
    /// back to any task with pending permissions.
    pub fn active_permission_key(&self) -> Option<TaskName> {
        let focused = self.focused_perm_key();
        if self.permissions.peek(&focused).is_some() {
            return Some(focused);
        }
        self.permissions
            .task_names_with_pending()
            .into_iter()
            .next()
    }

    // ── Detail view ──────────────────────────────────────────────────

    /// Open a task's detail view: select it, show detail panel, reset scrolls,
    /// focus chat input, and restore that task's input buffer.
    /// Callers that need to preserve the *current* input should call
    /// `save_current_input()` **before** this method.
    pub fn open_task_detail(&mut self, index: usize) {
        let active = self.active_state_mut();
        // Get the task ID for the target index
        let task_id = active.task_list.tasks.get(index).map(|t| t.id.clone());
        active.task_list.list_state.select(Some(index));
        active.task_list.show_detail = true;
        active.task_list.detail_scroll = 0;
        active.chat_view.reset_scroll();
        if let Some(tid) = task_id {
            active.enter_task_detail(&tid);
        }
        self.focus = Focus::ChatInput;
    }

    /// Leave the task detail view and return to the main chat.
    /// Resets chat scroll and restores the main chat input buffer.
    pub fn close_task_detail(&mut self) {
        let active = self.active_state_mut();
        let task_id = active.task_list.selected_task().map(|t| t.id.clone());
        active.task_list.show_detail = false;
        active.chat_view.reset_scroll();
        if let Some(tid) = task_id {
            active.leave_task_detail(&tid);
        }
        self.focus = Focus::ChatInput;
    }

    /// Move to the next (forward=true) or previous (forward=false) task
    /// from within the task chat input. Returns `true` if navigation stayed
    /// within bounds, `false` if it wrapped past the edge (detail is hidden).
    pub fn navigate_to_adjacent_task(&mut self, forward: bool) -> bool {
        let active = self.active_state_mut();
        let old_task_id = active.task_list.selected_task().map(|t| t.id.clone());
        active.chat_view.reset_scroll();
        let current = active.task_list.list_state.selected().unwrap_or(0);
        let in_bounds = if forward {
            if current + 1 < active.task_list.tasks.len() {
                active.task_list.list_state.select(Some(current + 1));
                active.task_list.detail_scroll = 0;
                true
            } else {
                active.task_list.show_detail = false;
                false
            }
        } else if current > 0 {
            active.task_list.list_state.select(Some(current - 1));
            active.task_list.detail_scroll = 0;
            true
        } else {
            active.task_list.show_detail = false;
            false
        };
        let new_task_id = active.task_list.selected_task().map(|t| t.id.clone());
        // Switch input buffers between old and new tasks
        if active.task_list.show_detail {
            if let (Some(old), Some(new)) = (&old_task_id, &new_task_id)
                && old != new
            {
                active.switch_task_detail(old, new);
            }
        } else if let Some(old) = &old_task_id {
            // Leaving detail view
            active.leave_task_detail(old);
        }
        in_bounds
    }

    // ── Project switching ────────────────────────────────────────────

    /// Switch to a project (or ExO when `project` is `None`).
    /// State persists in `projects[pid]` — no save/restore needed.
    /// Just sets IDs, hides project list, loads tasks, and optionally focuses a task.
    pub fn switch_to_project(
        &mut self,
        project: Option<(ProjectName, ProjectId)>,
        tasks: Vec<Task>,
        focus_task: Option<&TaskName>,
    ) {
        // Remember current project for Ctrl+R
        self.last_project_id = self.active_project_id.take();
        self.active_project_name = project.as_ref().map(|(n, _)| n.clone());
        self.active_project_id = project.map(|(_, id)| id);
        self.project_list.show_projects = false;
        self.focus = Focus::ChatInput;

        // Load tasks into target workspace
        let active = self.active_state_mut();
        active.task_list.show_detail = false;
        active.chat_view.reset_scroll();
        active.task_list.refresh_tasks(tasks);

        // Focus a specific task if requested
        if let Some(name) = focus_task {
            let active = self.active_state_mut();
            if let Some(pos) = active.task_list.tasks.iter().position(|t| &t.name == name) {
                active.task_list.list_state.select(Some(pos));
                active.task_list.show_detail = true;
                active.task_list.detail_scroll = 0;
            }
        }
    }

    // ── Project list ─────────────────────────────────────────────────

    /// Show the project list overlay, replacing the task list.
    pub fn show_project_list(&mut self, projects: Vec<Project>) {
        self.project_list.projects = projects;
        if !self.project_list.projects.is_empty() {
            self.project_list.list_state.select(Some(0));
        }
        self.project_list.show_projects = true;
        self.focus = Focus::ProjectList;
    }

    pub fn next_project(&mut self) {
        self.project_list.next_project();
    }

    pub fn previous_project(&mut self) {
        self.project_list.previous_project();
    }

    // ── Search ───────────────────────────────────────────────────────

    /// Enter search mode: clear the search input and focus the search bar.
    pub fn enter_search_mode(&mut self) {
        self.search_input.take();
        self.update_search_filter();
        self.focus = Focus::TaskSearch;
    }

    /// Exit search mode: clear filters, clamp selection, restore focus.
    pub fn exit_search(&mut self) {
        self.search_input.take();
        if self.project_list.show_projects {
            self.project_list.filtered_project_indices.clear();
            if !self.project_list.projects.is_empty() {
                let sel = self
                    .project_list
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.project_list.projects.len() - 1);
                self.project_list.list_state.select(Some(sel));
            }
            self.focus = Focus::ProjectList;
        } else {
            let active = self.active_state_mut();
            active.task_list.filtered_indices.clear();
            if !active.task_list.tasks.is_empty() {
                let sel = active
                    .task_list
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(active.task_list.tasks.len() - 1);
                active.task_list.list_state.select(Some(sel));
            }
            self.focus = Focus::TaskList;
        }
    }

    /// Confirm the current search selection. For tasks, opens the detail view.
    /// For projects, selects the project. Returns `true` if a selection was made.
    pub fn confirm_search_selection(&mut self) -> bool {
        if self.project_list.show_projects {
            if let Some(real_idx) = self.selected_filtered_project_index() {
                self.project_list.list_state.select(Some(real_idx));
            }
            self.search_input.take();
            self.project_list.filtered_project_indices.clear();
            self.focus = Focus::ProjectList;
            true
        } else {
            let selected = if let Some(real_idx) = self.selected_filtered_task_index() {
                self.open_task_detail(real_idx);
                true
            } else {
                self.focus = Focus::TaskList;
                false
            };
            self.search_input.take();
            self.active_state_mut().task_list.filtered_indices.clear();
            selected
        }
    }

    /// Recompute `filtered_indices` based on `search_query`.
    /// Fuzzy match: each query char must appear in order (e.g. "res" matches "r.*e.*s.*").
    pub fn update_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        let active = self.active_state_mut();
        active.task_list.filtered_indices = active
            .task_list
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if query.is_empty() {
                    return true;
                }
                let name = t.name.as_str().to_lowercase();
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
        // Clamp selection to filtered range
        if active.task_list.filtered_indices.is_empty() {
            active.task_list.list_state.select(None);
        } else {
            let sel = active.task_list.list_state.selected().unwrap_or(0);
            if let Some(filtered_pos) = active
                .task_list
                .filtered_indices
                .iter()
                .position(|&i| i == sel)
            {
                active.task_list.list_state.select(Some(filtered_pos));
            } else {
                active.task_list.list_state.select(Some(0));
            }
        }
    }

    pub fn update_project_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        self.project_list.filtered_project_indices = self
            .project_list
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
        if self.project_list.filtered_project_indices.is_empty() {
            self.project_list.list_state.select(None);
        } else {
            let sel = self.project_list.list_state.selected().unwrap_or(0);
            if let Some(pos) = self
                .project_list
                .filtered_project_indices
                .iter()
                .position(|&i| i == sel)
            {
                self.project_list.list_state.select(Some(pos));
            } else {
                self.project_list.list_state.select(Some(0));
            }
        }
    }

    pub fn search_next_project(&mut self) {
        self.project_list.search_next_project();
    }

    pub fn search_prev_project(&mut self) {
        self.project_list.search_prev_project();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{ProjectId, ProjectName, TaskId, TaskName};
    use crate::task::{Project, Task};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::Path;

    fn make_task(name: &str) -> Task {
        Task::new(
            TaskId::generate(),
            TaskName::from(name.to_string()),
            "engineer",
            &HashMap::new(),
            Path::new("/tmp"),
            None,
        )
    }

    fn make_project(name: &str) -> Project {
        Project {
            id: ProjectId::generate(),
            name: ProjectName::from(name.to_string()),
            description: String::new(),
            created_at: Utc::now(),
        }
    }

    fn state_with_tasks(n: usize) -> ScreenState {
        let tasks: Vec<Task> = (0..n).map(|i| make_task(&format!("task-{i}"))).collect();
        let exo = ProjectState::new(crate::tui::chat::AssistantChat::new(), tasks);
        ScreenState::new(exo)
    }

    // ── scroll_detail_up / scroll_detail_down ───────────────────────

    #[test]
    fn scroll_detail_down_increments() {
        let mut s = state_with_tasks(0);
        s.scroll_detail_down();
        assert_eq!(s.active_state().task_list.detail_scroll, 10);
        s.scroll_detail_down();
        assert_eq!(s.active_state().task_list.detail_scroll, 20);
    }

    #[test]
    fn scroll_detail_up_decrements() {
        let mut s = state_with_tasks(0);
        s.active_state_mut().task_list.detail_scroll = 25;
        s.scroll_detail_up();
        assert_eq!(s.active_state().task_list.detail_scroll, 15);
    }

    #[test]
    fn scroll_detail_up_saturates_at_zero() {
        let mut s = state_with_tasks(0);
        s.active_state_mut().task_list.detail_scroll = 5;
        s.scroll_detail_up();
        assert_eq!(s.active_state().task_list.detail_scroll, 0);
    }

    // ── open_task_detail ────────────────────────────────────────────

    #[test]
    fn open_task_detail_sets_expected_fields() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.show_detail = false;
        s.active_state_mut().task_list.detail_scroll = 99;
        s.active_state_mut().chat_view.scroll_chat_up(); // set non-zero scroll
        s.focus = Focus::TaskList;

        s.open_task_detail(2);

        let active = s.active_state();
        assert!(active.task_list.show_detail);
        assert_eq!(active.task_list.list_state.selected(), Some(2));
        assert_eq!(active.task_list.detail_scroll, 0);
        assert_eq!(active.chat_view.chat_scroll(), 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── close_task_detail ───────────────────────────────────────────

    #[test]
    fn close_task_detail_hides_and_resets() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail = true;
        s.active_state_mut().chat_view.scroll_chat_up(); // set non-zero scroll
        s.focus = Focus::TaskList;

        s.close_task_detail();

        let active = s.active_state();
        assert!(!active.task_list.show_detail);
        assert_eq!(active.chat_view.chat_scroll(), 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── navigate_to_adjacent_task ───────────────────────────────────

    #[test]
    fn navigate_forward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.show_detail = true;
        s.active_state_mut().task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(true);

        assert!(result);
        let active = s.active_state();
        assert_eq!(active.task_list.list_state.selected(), Some(1));
        assert!(active.task_list.show_detail);
        assert_eq!(active.task_list.detail_scroll, 0);
    }

    #[test]
    fn navigate_forward_past_end_hides_detail() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail = true;
        s.active_state_mut().task_list.list_state.select(Some(1));

        let result = s.navigate_to_adjacent_task(true);

        assert!(!result);
        assert!(!s.active_state().task_list.show_detail);
    }

    #[test]
    fn navigate_backward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.show_detail = true;
        s.active_state_mut().task_list.list_state.select(Some(2));

        let result = s.navigate_to_adjacent_task(false);

        assert!(result);
        let active = s.active_state();
        assert_eq!(active.task_list.list_state.selected(), Some(1));
        assert!(active.task_list.show_detail);
    }

    #[test]
    fn navigate_backward_past_start_hides_detail() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail = true;
        s.active_state_mut().task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(false);

        assert!(!result);
        assert!(!s.active_state().task_list.show_detail);
    }

    // ── show_project_list ───────────────────────────────────────────

    #[test]
    fn show_project_list_sets_state() {
        let mut s = state_with_tasks(0);
        let projects = vec![make_project("alpha"), make_project("beta")];

        s.show_project_list(projects);

        assert!(s.project_list.show_projects);
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), Some(0));
        assert_eq!(s.project_list.projects.len(), 2);
    }

    #[test]
    fn show_project_list_empty() {
        let mut s = state_with_tasks(0);

        s.show_project_list(vec![]);

        assert!(s.project_list.show_projects);
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), None);
    }

    // ── refresh_projects ────────────────────────────────────────────

    #[test]
    fn refresh_projects_clamps_selection() {
        let mut s = state_with_tasks(0);
        s.project_list.projects = vec![make_project("a"), make_project("b"), make_project("c")];
        s.project_list.list_state.select(Some(2));

        s.refresh_projects(vec![make_project("a"), make_project("b")]);

        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    #[test]
    fn refresh_projects_empty_clears_selection() {
        let mut s = state_with_tasks(0);
        s.project_list.list_state.select(Some(1));

        s.refresh_projects(vec![]);

        assert_eq!(s.project_list.list_state.selected(), None);
    }

    // ── enter_search_mode ───────────────────────────────────────────

    #[test]
    fn enter_search_mode_focuses_search() {
        let mut s = state_with_tasks(2);
        s.focus = Focus::TaskList;
        s.search_input.set("old query");

        s.enter_search_mode();

        assert!(matches!(s.focus, Focus::TaskSearch));
        assert!(s.search_input.buffer().is_empty());
    }

    // ── exit_search ─────────────────────────────────────────────────

    #[test]
    fn exit_search_tasks_restores_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.focus = Focus::TaskSearch;
        s.active_state_mut().task_list.filtered_indices = vec![0, 2];
        s.active_state_mut().task_list.list_state.select(Some(0));

        s.exit_search();

        assert!(matches!(s.focus, Focus::TaskList));
        assert!(s.active_state().task_list.filtered_indices.is_empty());
    }

    #[test]
    fn exit_search_projects_restores_project_list() {
        let mut s = state_with_tasks(0);
        s.project_list.show_projects = true;
        s.project_list.projects = vec![make_project("a"), make_project("b")];
        s.focus = Focus::TaskSearch;
        s.project_list.filtered_project_indices = vec![0, 1];
        s.project_list.list_state.select(Some(1));

        s.exit_search();

        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_project_indices.is_empty());
        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    // ── confirm_search_selection ────────────────────────────────────

    #[test]
    fn confirm_search_selection_opens_task_detail() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.active_state_mut().task_list.filtered_indices = vec![0, 2];
        s.active_state_mut().task_list.list_state.select(Some(1)); // points to filtered_indices[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        let active = s.active_state();
        assert!(active.task_list.show_detail);
        assert_eq!(active.task_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ChatInput));
        assert!(s.active_state().task_list.filtered_indices.is_empty());
    }

    #[test]
    fn confirm_search_selection_no_match_goes_to_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.active_state_mut().task_list.filtered_indices = vec![];
        s.active_state_mut().task_list.list_state.select(None);

        let result = s.confirm_search_selection();

        assert!(!result);
        assert!(matches!(s.focus, Focus::TaskList));
    }

    #[test]
    fn confirm_search_selection_project() {
        let mut s = state_with_tasks(0);
        s.project_list.show_projects = true;
        s.project_list.projects = vec![make_project("a"), make_project("b"), make_project("c")];
        s.project_list.filtered_project_indices = vec![0, 2];
        s.project_list.list_state.select(Some(1)); // points to filtered[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        assert_eq!(s.project_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_project_indices.is_empty());
    }
}

use std::collections::HashMap;
use std::path::PathBuf;

use crate::primitives::{ProjectId, ProjectName, TaskName};
use crate::project::Project;
use crate::task::Task;
use crate::tui::keybindings::Keybindings;

use super::TaskListState;
use super::project_list::ProjectListState;
use super::project_state::ProjectState;
use super::{Focus, InputState, PermissionStore};
use crate::tui::permissions::ActivePermission;

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
    last_project_id: Option<ProjectId>,

    pub project_list: ProjectListState,
    /// Whether ExO prefers to show the project list (true) or task list (false).
    /// Persists across navigation so returning to ExO restores the right panel.
    exo_show_project_list: bool,
    should_quit: bool,
    pub(in crate::tui) focus: Focus,
    pub permissions: PermissionStore,
    /// Transient error message shown in the prompt bar. Cleared on next keypress.
    status_error: Option<String>,
    /// Input state for the task search filter.
    pub search_input: InputState,
    /// Global map of task_name → project_id for all running tasks.
    /// Updated every tick from the full (unscoped) active task list.
    global_task_projects: HashMap<TaskName, Option<ProjectId>>,
    /// Global list of (task_name, work_dir) for all running tasks.
    /// Used for CWD→task matching in permission/resolved/idle handlers
    /// so lookups work regardless of which project is currently displayed.
    global_task_work_dirs: Vec<(TaskName, String)>,
    /// User-configurable keybindings.
    pub keybindings: Keybindings,
}

impl ScreenState {
    pub fn new(exo: ProjectState, keybindings: Keybindings) -> Self {
        Self {
            exo,
            projects: HashMap::new(),
            active_project_id: None,
            active_project_name: None,
            last_project_id: None,
            project_list: ProjectListState::new(),
            exo_show_project_list: true,
            should_quit: false,
            focus: Focus::ChatInput,
            permissions: PermissionStore::new(),
            status_error: None,
            search_input: InputState::new(),
            global_task_projects: HashMap::new(),
            global_task_work_dirs: Vec::new(),
            keybindings,
        }
    }

    /// Add a project workspace.
    pub fn add_project(&mut self, project_id: ProjectId, project_state: ProjectState) {
        self.projects.insert(project_id, project_state);
    }

    // ── Quit ─────────────────────────────────────────────────────────

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    // ── Focus ────────────────────────────────────────────────────────

    pub fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    pub fn focus_on_tasks(&mut self) {
        // Check visibility before hiding so we can update the ExO preference.
        let was_project_list = self.project_list.is_visible();
        self.project_list.hide();
        // Update ExO preference: either closing project list or already in ExO.
        if was_project_list || self.active_project_id.is_none() {
            self.exo_show_project_list = false;
        }
        self.focus = Focus::TaskList;
    }

    pub fn focus_right(&mut self) {
        if self.project_list.is_visible() {
            self.focus = Focus::ProjectList;
        } else {
            self.focus = Focus::TaskList;
        }
    }

    // ── Last project ────────────────────────────────────────────────

    pub fn last_project_id(&self) -> Option<&ProjectId> {
        self.last_project_id.as_ref()
    }

    // ── Status error ─────────────────────────────────────────────────

    pub fn set_status_error(&mut self, msg: String) {
        self.status_error = Some(msg);
    }

    pub fn clear_status_error(&mut self) {
        self.status_error = None;
    }

    pub fn status_error(&self) -> Option<&str> {
        self.status_error.as_deref()
    }

    // ── Global task mappings ─────────────────────────────────────────

    pub fn update_global_task_mappings(
        &mut self,
        projects: HashMap<TaskName, Option<ProjectId>>,
        work_dirs: Vec<(TaskName, String)>,
    ) {
        self.global_task_projects = projects;
        self.global_task_work_dirs = work_dirs;
    }

    pub fn global_task_project(&self, name: &TaskName) -> Option<&Option<ProjectId>> {
        self.global_task_projects.get(name)
    }

    pub fn render_loop_starting(&mut self, projects: Vec<Project>) {
        self.exo.reset_tasks_to_idle();
        for project in self.projects.values_mut() {
            project.reset_tasks_to_idle();
        }
        // Show project list on startup if ExO prefers it (default: true).
        if self.exo_show_project_list && !projects.is_empty() {
            self.show_project_list(projects);
            // Override focus — start in chat, not project list.
            self.focus = Focus::ChatInput;
        }
    }

    // ── Hook event helpers ─────────────────────────────────────────────

    /// Resolve a CWD to a task name using the global work-dir map.
    fn task_name_for_cwd(&self, cwd: &str) -> Option<TaskName> {
        let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
        self.global_task_work_dirs
            .iter()
            .find(|(_, wd)| {
                let canon = std::fs::canonicalize(wd).unwrap_or_else(|_| PathBuf::from(wd));
                resolved.starts_with(&canon)
            })
            .map(|(name, _)| name.clone())
    }

    /// Find the TaskListState that contains a task with the given name.
    fn task_list_for_task_mut(&mut self, name: &TaskName) -> Option<&mut TaskListState> {
        if let Some(tl) = self.exo.task_list_for_name(name) {
            return Some(tl);
        }
        for ps in self.projects.values_mut() {
            if let Some(tl) = ps.task_list_for_name(name) {
                return Some(tl);
            }
        }
        None
    }

    /// Mark the pane for a task (identified by CWD) as active, take its
    /// pending permission, and return it. Returns `None` if CWD doesn't
    /// match any task or there is no pending permission.
    pub fn resolve_permission(&mut self, cwd: &str) -> Option<ActivePermission> {
        let name = self.task_name_for_cwd(cwd)?;
        if let Some(task_list) = self.task_list_for_task_mut(&name) {
            task_list.activate_task_pane(&name);
        }
        self.permissions.take(&name)
    }

    /// Mark the pane for a task (identified by CWD) as idle.
    /// Returns `Some(task_name)` if the task was newly marked idle (for notification).
    pub fn mark_task_idle(&mut self, cwd: &str) -> Option<TaskName> {
        if let Some(name) = self.task_name_for_cwd(cwd)
            && let Some(task_list) = self.task_list_for_task_mut(&name)
            && task_list.idle_task_pane(&name)
        {
            Some(name)
        } else {
            None
        }
    }

    /// Mark the pane for a task (identified by CWD) as active.
    /// Returns `Some(task_name)` if the task was newly marked active (for notification).
    pub fn mark_task_active(&mut self, cwd: &str) -> Option<TaskName> {
        if let Some(name) = self.task_name_for_cwd(cwd)
            && let Some(task_list) = self.task_list_for_task_mut(&name)
            && task_list.activate_task_pane(&name)
        {
            Some(name)
        } else {
            None
        }
    }

    /// Resolve a CWD to its task name, falling back to the given default.
    pub fn task_name_for_cwd_or(&self, cwd: &str, default: TaskName) -> TaskName {
        self.task_name_for_cwd(cwd).unwrap_or(default)
    }

    /// Mark a task's pane as active by task name.
    pub fn mark_task_active_by_name(&mut self, name: &TaskName) {
        if let Some(task_list) = self.task_list_for_task_mut(name) {
            task_list.activate_task_pane(name);
        }
    }

    /// Whether any project (including ExO) is currently streaming.
    pub fn any_streaming(&self) -> bool {
        self.exo.chat_view.assistant.streaming
            || self
                .projects
                .values()
                .any(|ps| ps.chat_view.assistant.streaming)
    }

    // ── Active state accessors ───────────────────────────────────────

    /// Get a reference to the active project state (ExO or current project).
    pub fn active_state(&self) -> &ProjectState {
        match &self.active_project_id {
            Some(pid) => self.projects.get(pid).unwrap_or(&self.exo),
            None => &self.exo,
        }
    }

    /// State to display in the chat panel. When the project list is visible
    /// and a project is highlighted, show that project's PM chat instead of
    /// the active task chat.
    pub fn chat_display_state(&self) -> (&ProjectState, Option<&str>) {
        let name = self.active_project_name.as_ref().map(|n| n.as_str());
        (self.active_state(), name)
    }

    /// Get a mutable reference to the active project state.
    pub fn active_state_mut(&mut self) -> &mut ProjectState {
        match &self.active_project_id {
            Some(pid) => {
                let pid = *pid;
                self.projects.get_mut(&pid).unwrap_or(&mut self.exo)
            }
            None => &mut self.exo,
        }
    }

    // ── Cycle next/prev (Tab / Shift-Tab) ──────────────────────────────

    /// Cycle to the next item in whichever list is active.
    /// Projects list visible → next project. Task detail open → next task.
    /// Otherwise → open first task detail.
    pub fn cycle_next(&mut self) {
        if self.project_list.is_visible() {
            self.next_project();
        } else if self.active_state().task_list.is_detail_visible() {
            self.navigate_to_adjacent_task(true);
        } else {
            self.open_first_task_detail();
        }
    }

    /// Cycle to the previous item in whichever list is active.
    /// Projects list visible → previous project. Task detail open → previous task.
    /// Otherwise → open last task detail.
    pub fn cycle_prev(&mut self) {
        if self.project_list.is_visible() {
            self.previous_project();
        } else if self.active_state().task_list.is_detail_visible() {
            self.navigate_to_adjacent_task(false);
        } else {
            self.open_last_task_detail();
        }
    }

    // ── Delegates to active TaskListState ─────────────────────────────

    pub fn selected_task(&self) -> Option<&Task> {
        self.active_state().task_list.selected_task()
    }

    pub fn next_task_with_detail(&mut self) {
        let active = self.active_state_mut();
        active.task_list.next();
        active.task_list.show_detail();
    }

    pub fn previous_task_with_detail(&mut self) {
        let active = self.active_state_mut();
        active.task_list.previous();
        active.task_list.show_detail();
    }

    pub fn hide_active_detail(&mut self) {
        self.active_state_mut().task_list.hide_detail();
    }

    pub fn search_next(&mut self) {
        if self.project_list.is_visible() {
            self.project_list.search_next_project();
        } else {
            self.active_state_mut().task_list.search_next();
        }
    }

    pub fn search_prev(&mut self) {
        if self.project_list.is_visible() {
            self.project_list.search_prev_project();
        } else {
            self.active_state_mut().task_list.search_prev();
        }
    }

    pub fn refresh_tasks(&mut self, tasks: Vec<Task>) {
        self.active_state_mut().task_list.refresh_tasks(tasks);
    }

    pub fn scroll_down_tasks(&mut self) {
        self.active_state_mut().task_list.scroll_down_tasks();
    }

    pub fn scroll_up_tasks(&mut self) {
        self.active_state_mut().task_list.scroll_up_tasks();
    }

    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.active_state().task_list.selected_filtered_task_index()
    }

    pub fn open_selected_task(&mut self) {
        if let Some(idx) = self.active_state().task_list.list_state.selected() {
            self.open_task_detail(idx);
        }
    }

    /// If the selected task is running, enter the close-task confirmation flow.
    pub fn confirm_close_selected_task(&mut self) {
        if let Some(task) = self.selected_task()
            && task.status.is_running()
        {
            let id = task.id;
            self.set_focus(Focus::ConfirmCloseTask(id));
        }
    }

    /// Enter the delete confirmation flow for the selected task.
    pub fn confirm_delete_selected_task(&mut self) {
        if let Some(task) = self.selected_task() {
            let id = task.id;
            self.set_focus(Focus::ConfirmDelete(id));
        }
    }

    pub fn focus_left(&mut self) {
        self.set_focus(Focus::ChatInput);
    }

    pub fn move_focus_up(&mut self) {
        self.set_focus(Focus::ChatHistory);
    }

    pub fn is_project_selected(&self) -> bool {
        self.active_project_name.is_some()
    }

    pub fn confirm_close_project(&mut self) {
        self.set_focus(Focus::ConfirmCloseProject);
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

    /// Focus the task list. If a task is selected, show its detail panel.
    pub fn focus_task_list_with_detail(&mut self) {
        if self.project_list.is_visible() {
            self.focus = Focus::ProjectList;
        } else {
            self.focus = Focus::TaskList;
            let active = self.active_state_mut();
            if active.task_list.list_state.selected().is_some() {
                active.task_list.show_detail();
            }
        }
    }

    // ── Paste ─────────────────────────────────────────────────────────

    pub fn accept_paste(&mut self, text: String) {
        if matches!(self.focus, Focus::ChatInput) {
            self.active_state_mut().input.accept_paste(text);
        }
    }

    // ── Delegates to active ChatViewState ────────────────────────────

    pub fn update_chat_viewport_height(&mut self, area_height: u16) {
        self.active_state_mut()
            .chat_view
            .update_chat_viewport_height(area_height);
    }

    pub fn scroll_chat_panel_up(&mut self) {
        self.active_state_mut().scroll_chat_panel_up();
    }

    pub fn scroll_chat_panel_down(&mut self) {
        self.active_state_mut().scroll_chat_panel_down();
    }

    /// If the active chat is streaming, finish it. Reset scroll regardless.
    pub fn cancel_streaming(&mut self) {
        let active = self.active_state_mut();
        if active.chat_view.assistant.streaming {
            active.chat_view.assistant.finish_streaming();
        }
        active.chat_view.reset_scroll();
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
            self.project_list.projects(),
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
        if active.task_list.is_detail_visible() {
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
        let task_id = active.task_list.tasks.get(index).map(|t| t.id);
        active.task_list.list_state.select(Some(index));
        active.task_list.show_detail();
        active.chat_view.reset_scroll();
        if let Some(tid) = task_id {
            active.enter_task_detail(&tid);
        }
        self.focus = Focus::ChatInput;
    }

    /// Open the first task's detail, or reset chat scroll if no tasks.
    pub fn open_first_task_detail(&mut self) {
        if !self.active_state().task_list.tasks.is_empty() {
            self.open_task_detail(0);
        } else {
            self.active_state_mut().chat_view.reset_scroll();
        }
    }

    /// Open the last task's detail, or reset chat scroll if no tasks.
    pub fn open_last_task_detail(&mut self) {
        let last = self.active_state().task_list.tasks.len().checked_sub(1);
        if let Some(idx) = last {
            self.open_task_detail(idx);
        } else {
            self.active_state_mut().chat_view.reset_scroll();
        }
    }

    /// Leave the task detail view and return to the main chat.
    /// Resets chat scroll and restores the main chat input buffer.
    pub fn close_task_detail(&mut self) {
        let active = self.active_state_mut();
        let task_id = active.task_list.selected_task().map(|t| t.id);
        active.task_list.hide_detail();
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
        let old_task_id = active.task_list.selected_task().map(|t| t.id);
        active.chat_view.reset_scroll();
        let current = active.task_list.list_state.selected().unwrap_or(0);
        let in_bounds = if forward {
            if current + 1 < active.task_list.tasks.len() {
                active.task_list.list_state.select(Some(current + 1));
                active.task_list.show_detail();
                true
            } else {
                active.task_list.hide_detail();
                false
            }
        } else if current > 0 {
            active.task_list.list_state.select(Some(current - 1));
            active.task_list.show_detail();
            true
        } else {
            active.task_list.hide_detail();
            false
        };
        let new_task_id = active.task_list.selected_task().map(|t| t.id);
        // Switch input buffers between old and new tasks
        if active.task_list.is_detail_visible() {
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
        // Save ExO's project-list preference before leaving.
        // When the project list is visible, sync_active_to_selected_project may
        // have set active_project_id, but we're still in ExO context.
        let leaving_exo = self.active_project_id.is_none() || self.project_list.is_visible();
        if leaving_exo {
            self.exo_show_project_list = self.project_list.is_visible();
        }

        // Remember current project for Ctrl+R
        self.last_project_id = self.active_project_id.take();
        self.active_project_name = project.as_ref().map(|(n, _)| n.clone());
        self.active_project_id = project.map(|(_, id)| id);

        self.project_list.hide();

        // Load tasks into target workspace (before restore may change active via sync)
        let active = self.active_state_mut();
        active.task_list.hide_detail();
        active.chat_view.reset_scroll();
        active.task_list.refresh_tasks(tasks);

        // Focus a specific task if requested
        if let Some(name) = focus_task {
            let active = self.active_state_mut();
            if let Some(pos) = active.task_list.tasks.iter().position(|t| &t.name == name) {
                active.task_list.list_state.select(Some(pos));
                active.task_list.show_detail();
            }
        }

        // Restore ExO's project-list preference when returning to ExO.
        // Skip when focusing a specific task (e.g. cycling permissions).
        if self.active_project_id.is_none() && self.exo_show_project_list && focus_task.is_none() {
            self.project_list.set_visible(true);
            // Don't sync active project — keep ExO chat visible.
            self.focus = Focus::ProjectList;
        } else {
            self.focus = Focus::ChatInput;
        }
    }

    // ── Project list ─────────────────────────────────────────────────

    /// Show the project list overlay, replacing the task list.
    /// Does NOT change the active project — ExO chat stays visible.
    /// Navigation (j/k) syncs the active project via next_project/previous_project.
    pub fn show_project_list(&mut self, projects: Vec<Project>) {
        // Record ExO preference BEFORE any state changes.
        self.exo_show_project_list = true;
        self.project_list.show(projects);
        // Don't sync active project — keep ExO chat visible until user navigates.
        self.focus = Focus::ProjectList;
    }

    pub fn next_project(&mut self) {
        self.project_list.next_project();
        self.sync_active_to_selected_project();
    }

    pub fn previous_project(&mut self) {
        self.project_list.previous_project();
        self.sync_active_to_selected_project();
    }

    fn sync_active_to_selected_project(&mut self) {
        if let Some(project) = self.project_list.selected_project() {
            self.active_project_id = Some(project.id);
            self.active_project_name = Some(project.name.clone());
        }
    }

    // ── Search ───────────────────────────────────────────────────────

    /// Enter search mode: clear the search input and focus the search bar.
    pub fn enter_search_mode(&mut self) {
        self.search_input.take();
        self.update_search_filter();
        self.focus = Focus::ListSearch;
    }

    /// Exit search mode: clear filters, clamp selection, restore focus.
    pub fn exit_search(&mut self) {
        self.search_input.take();
        if self.project_list.is_visible() {
            self.project_list.clear_filter();
            if !self.project_list.projects().is_empty() {
                let sel = self
                    .project_list
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.project_list.projects().len() - 1);
                self.project_list.list_state.select(Some(sel));
            }
            self.focus = Focus::ProjectList;
        } else {
            let active = self.active_state_mut();
            active.task_list.clear_filter();
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
        if self.project_list.is_visible() {
            if let Some(real_idx) = self.selected_filtered_project_index() {
                self.project_list.list_state.select(Some(real_idx));
            }
            self.search_input.take();
            self.project_list.clear_filter();
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
            self.active_state_mut().task_list.clear_filter();
            selected
        }
    }

    /// Recompute `filtered_indices` based on `search_query`.
    /// Dispatches to project or task filter depending on which list is visible.
    pub fn update_search_filter(&mut self) {
        if self.project_list.is_visible() {
            self.update_project_search_filter();
        } else {
            self.update_task_search_filter();
        }
    }

    fn update_task_search_filter(&mut self) {
        let query = self.search_input.char_vec();
        self.active_state_mut().task_list.filter(&query);
    }

    fn update_project_search_filter(&mut self) {
        let query = self.search_input.char_vec();
        self.project_list.filter(&query);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{ClaudeSessionId, ProjectId, ProjectName, TaskId, TaskName};
    use crate::project::NewProject;
    use crate::task::{NewTask, Task};
    use es_entity::*;

    fn make_task(name: &str) -> Task {
        let new_task = NewTask {
            id: TaskId::new(),
            name: TaskName::from(name.to_string()),
            skill_name: "engineer".to_string(),
            params_json: "{}".to_string(),
            work_dir: Some("/tmp".to_string()),
            session_id: ClaudeSessionId::new(),
            project_id: None,
        };
        let events = new_task.into_events();
        Task::try_from_events(events).unwrap()
    }

    fn make_project(name: &str) -> Project {
        let new = NewProject {
            id: ProjectId::new(),
            name: ProjectName::from(name.to_string()),
            description: String::new(),
        };
        let events = new.into_events();
        Project::try_from_events(events).unwrap()
    }

    fn state_with_tasks(n: usize) -> ScreenState {
        let tasks: Vec<Task> = (0..n).map(|i| make_task(&format!("task-{i}"))).collect();
        let exo = ProjectState::new(crate::tui::chat::AssistantChat::new(), tasks);
        ScreenState::new(exo, Keybindings::default())
    }

    // ── scroll_up_tasks / scroll_down_tasks ───────────────────────

    #[test]
    fn scroll_down_tasks_increments() {
        let mut s = state_with_tasks(0);
        s.scroll_down_tasks();
        assert_eq!(s.active_state().task_list.detail_scroll(), 10);
        s.scroll_down_tasks();
        assert_eq!(s.active_state().task_list.detail_scroll(), 20);
    }

    #[test]
    fn scroll_up_tasks_decrements() {
        let mut s = state_with_tasks(0);
        // Scroll down first to set a value we can decrement
        s.scroll_down_tasks(); // 10
        s.scroll_down_tasks(); // 20
        s.scroll_down_tasks(); // 30 (close enough to 25)
        s.scroll_up_tasks();
        assert_eq!(s.active_state().task_list.detail_scroll(), 20);
    }

    #[test]
    fn scroll_up_tasks_saturates_at_zero() {
        let mut s = state_with_tasks(0);
        s.scroll_down_tasks(); // 10
        s.scroll_up_tasks(); // 0
        s.scroll_up_tasks(); // still 0
        assert_eq!(s.active_state().task_list.detail_scroll(), 0);
    }

    // ── open_task_detail ────────────────────────────────────────────

    #[test]
    fn open_task_detail_sets_expected_fields() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.hide_detail();
        s.active_state_mut().chat_view.scroll_chat_up(); // set non-zero scroll
        s.focus = Focus::TaskList;

        s.open_task_detail(2);

        let active = s.active_state();
        assert!(active.task_list.is_detail_visible());
        assert_eq!(active.task_list.list_state.selected(), Some(2));
        assert_eq!(active.task_list.detail_scroll(), 0);
        assert_eq!(active.chat_view.chat_scroll(), 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── close_task_detail ───────────────────────────────────────────

    #[test]
    fn close_task_detail_hides_and_resets() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail();
        s.active_state_mut().chat_view.scroll_chat_up(); // set non-zero scroll
        s.focus = Focus::TaskList;

        s.close_task_detail();

        let active = s.active_state();
        assert!(!active.task_list.is_detail_visible());
        assert_eq!(active.chat_view.chat_scroll(), 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── navigate_to_adjacent_task ───────────────────────────────────

    #[test]
    fn navigate_forward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.show_detail();
        s.active_state_mut().task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(true);

        assert!(result);
        let active = s.active_state();
        assert_eq!(active.task_list.list_state.selected(), Some(1));
        assert!(active.task_list.is_detail_visible());
        assert_eq!(active.task_list.detail_scroll(), 0);
    }

    #[test]
    fn navigate_forward_past_end_hides_detail() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail();
        s.active_state_mut().task_list.list_state.select(Some(1));

        let result = s.navigate_to_adjacent_task(true);

        assert!(!result);
        assert!(!s.active_state().task_list.is_detail_visible());
    }

    #[test]
    fn navigate_backward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.active_state_mut().task_list.show_detail();
        s.active_state_mut().task_list.list_state.select(Some(2));

        let result = s.navigate_to_adjacent_task(false);

        assert!(result);
        let active = s.active_state();
        assert_eq!(active.task_list.list_state.selected(), Some(1));
        assert!(active.task_list.is_detail_visible());
    }

    #[test]
    fn navigate_backward_past_start_hides_detail() {
        let mut s = state_with_tasks(2);
        s.active_state_mut().task_list.show_detail();
        s.active_state_mut().task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(false);

        assert!(!result);
        assert!(!s.active_state().task_list.is_detail_visible());
    }

    // ── show_project_list ───────────────────────────────────────────

    #[test]
    fn show_project_list_sets_state() {
        let mut s = state_with_tasks(0);
        let projects = vec![make_project("alpha"), make_project("beta")];

        s.show_project_list(projects);

        assert!(s.project_list.is_visible());
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), Some(0));
        assert_eq!(s.project_list.projects().len(), 2);
    }

    #[test]
    fn show_project_list_empty() {
        let mut s = state_with_tasks(0);

        s.show_project_list(vec![]);

        assert!(s.project_list.is_visible());
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), None);
    }

    // ── refresh_projects ────────────────────────────────────────────

    #[test]
    fn refresh_projects_clamps_selection() {
        let mut s = state_with_tasks(0);
        s.project_list.set_projects(vec![
            make_project("a"),
            make_project("b"),
            make_project("c"),
        ]);
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

        assert!(matches!(s.focus, Focus::ListSearch));
        assert!(s.search_input.buffer().is_empty());
    }

    // ── exit_search ─────────────────────────────────────────────────

    #[test]
    fn exit_search_tasks_restores_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();
        s.focus = Focus::ListSearch;
        s.active_state_mut()
            .task_list
            .set_filtered_indices(vec![0, 2]);
        s.active_state_mut().task_list.list_state.select(Some(0));

        s.exit_search();

        assert!(matches!(s.focus, Focus::TaskList));
        assert!(s.active_state().task_list.filtered_indices().is_empty());
    }

    #[test]
    fn exit_search_projects_restores_project_list() {
        let mut s = state_with_tasks(0);
        s.project_list
            .show(vec![make_project("a"), make_project("b")]);
        s.focus = Focus::ListSearch;
        s.project_list.set_filtered_indices(vec![0, 1]);
        s.project_list.list_state.select(Some(1));

        s.exit_search();

        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_indices().is_empty());
        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    // ── confirm_search_selection ────────────────────────────────────

    #[test]
    fn confirm_search_selection_opens_task_detail() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();
        s.active_state_mut()
            .task_list
            .set_filtered_indices(vec![0, 2]);
        s.active_state_mut().task_list.list_state.select(Some(1)); // points to filtered_indices[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        let active = s.active_state();
        assert!(active.task_list.is_detail_visible());
        assert_eq!(active.task_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ChatInput));
        assert!(s.active_state().task_list.filtered_indices().is_empty());
    }

    #[test]
    fn confirm_search_selection_no_match_goes_to_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();
        s.active_state_mut().task_list.set_filtered_indices(vec![]);
        s.active_state_mut().task_list.list_state.select(None);

        let result = s.confirm_search_selection();

        assert!(!result);
        assert!(matches!(s.focus, Focus::TaskList));
    }

    #[test]
    fn confirm_search_selection_project() {
        let mut s = state_with_tasks(0);
        s.project_list.show(vec![
            make_project("a"),
            make_project("b"),
            make_project("c"),
        ]);
        s.project_list.set_filtered_indices(vec![0, 2]);
        s.project_list.list_state.select(Some(1)); // points to filtered[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        assert_eq!(s.project_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_indices().is_empty());
    }

    // ── cycle_next / cycle_prev ───────────────────────────────────

    #[test]
    fn cycle_next_with_project_list_cycles_projects() {
        let mut s = state_with_tasks(3);
        s.show_project_list(vec![make_project("a"), make_project("b")]);
        s.project_list.list_state.select(Some(0));

        s.cycle_next();

        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    #[test]
    fn cycle_prev_with_project_list_cycles_projects() {
        let mut s = state_with_tasks(3);
        s.show_project_list(vec![make_project("a"), make_project("b")]);
        s.project_list.list_state.select(Some(1));

        s.cycle_prev();

        assert_eq!(s.project_list.list_state.selected(), Some(0));
    }

    #[test]
    fn cycle_next_without_projects_opens_first_task() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();

        s.cycle_next();

        assert!(s.active_state().task_list.is_detail_visible());
        assert_eq!(s.active_state().task_list.list_state.selected(), Some(0));
    }

    #[test]
    fn cycle_prev_without_projects_opens_last_task() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();

        s.cycle_prev();

        assert!(s.active_state().task_list.is_detail_visible());
        assert_eq!(s.active_state().task_list.list_state.selected(), Some(2));
    }

    #[test]
    fn cycle_next_with_detail_navigates_tasks() {
        let mut s = state_with_tasks(3);
        s.project_list.hide();
        s.open_task_detail(0);

        s.cycle_next();

        assert_eq!(s.active_state().task_list.list_state.selected(), Some(1));
        assert!(s.active_state().task_list.is_detail_visible());
    }

    // ── switch_to_project restores project list ───────────────────

    #[test]
    fn switch_to_project_saves_and_restores_exo_project_list() {
        let mut s = state_with_tasks(0);
        let projects = vec![make_project("alpha"), make_project("beta")];
        let project_id = projects[0].id;
        let project_name = projects[0].name.clone();

        // Show project list in ExO
        s.show_project_list(projects);
        assert!(s.project_list.is_visible());

        // Navigate to a project (simulates Enter on project list)
        s.switch_to_project(Some((project_name, project_id)), vec![], None);
        assert!(!s.project_list.is_visible());

        // Return to ExO via Ctrl+O
        s.switch_to_project(None, vec![], None);
        assert!(s.project_list.is_visible());
        assert!(matches!(s.focus, Focus::ProjectList));
    }

    #[test]
    fn switch_to_project_no_restore_when_tasks_were_shown() {
        let mut s = state_with_tasks(0);
        let projects = vec![make_project("alpha")];
        let project_id = projects[0].id;
        let project_name = projects[0].name.clone();

        // Show project list then close it (user prefers tasks)
        s.show_project_list(projects);
        s.focus_on_tasks();
        assert!(!s.exo_show_project_list);

        // Navigate to a project
        s.switch_to_project(Some((project_name, project_id)), vec![], None);

        // Return to ExO — should NOT show project list
        s.switch_to_project(None, vec![], None);
        assert!(!s.project_list.is_visible());
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── focus_on_tasks updates ExO preference ─────────────────────

    #[test]
    fn focus_on_tasks_clears_exo_preference() {
        let mut s = state_with_tasks(0);
        s.show_project_list(vec![make_project("a")]);
        assert!(s.exo_show_project_list);

        s.focus_on_tasks();

        assert!(!s.exo_show_project_list);
        assert!(!s.project_list.is_visible());
    }

    // ── show_project_list sets ExO preference ─────────────────────

    #[test]
    fn show_project_list_sets_exo_preference() {
        let mut s = state_with_tasks(0);
        s.exo_show_project_list = false;

        s.show_project_list(vec![make_project("a")]);

        assert!(s.exo_show_project_list);
    }
}

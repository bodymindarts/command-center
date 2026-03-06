mod chat_view;
mod project_list;
mod project_state;
mod screen;
mod task_list;

use crate::primitives::{ProjectName, TaskId};

pub use chat_view::ChatViewState;
pub use project_list::ProjectListState;
pub use project_state::ProjectState;
pub use screen::ScreenState;
pub(super) use screen::log_hook;
pub use task_list::TaskListState;

pub use super::input::InputState;
pub use super::permissions::PermissionStore;

pub enum Focus {
    TaskList,
    ListSearch,
    ProjectList,
    ChatInput,
    ChatHistory,
    ConfirmDelete(TaskId),
    ConfirmDeleteProject(ProjectName),
    ConfirmCloseTask(TaskId),
    ConfirmCloseProject,
}

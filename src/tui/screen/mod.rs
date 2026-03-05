mod chat_view;
mod project_list;
mod state;
mod task_list;

use crate::primitives::{ProjectName, TaskId};

pub use chat_view::ChatViewState;
pub use project_list::ProjectListState;
pub use state::ScreenState;
pub use task_list::TaskListState;

pub use super::input::InputState;
pub use super::permissions::PermissionStore;

pub enum Focus {
    TaskList,
    TaskSearch,
    ProjectList,
    ChatInput,
    ChatHistory,
    ConfirmDelete(TaskId),
    ConfirmDeleteProject(ProjectName),
    ConfirmCloseTask(TaskId),
    ConfirmCloseProject,
}

//! Session-scoped todo-list tools.
//!
//! The extension lets an agent replace a task list, move individual tasks
//! through their lifecycle, inspect current progress, and clear the list.

use e_agent_extension::extension;

#[extension(
    description = "Manage a session-scoped todo list: create or replace the list, update item statuses, inspect all items, and clear the list.",
    system_prompt = "Use the todo tools proactively when working on complex, non-trivial, or multi-step tasks, when the user explicitly requests a todo list, or when the user provides multiple tasks. Skip them for a single straightforward task, work that takes fewer than three trivial steps, or purely conversational or informational requests. Create a concise list of specific, actionable tasks before starting work, and check the current list first to avoid duplicate tasks. Keep exactly one task in_progress while working unless tasks are being performed in parallel. Mark a task in_progress before starting it and completed immediately after it is fully finished; do not batch status updates or mark work completed while relevant tests or checks are failing. After completing a task, use list to check overall progress and identify the next pending task, and revise the list when new work is discovered or the plan changes. The item index is zero-based, and valid statuses are pending, in_progress, and completed. Every todo tool prints the complete current list as YAML; treat the latest printed snapshot as the source of truth. An empty list is set when session start."
)]
mod todo {
    use anyhow::{Context, Result};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// The lifecycle state of a todo item.
    ///
    /// New items start as `pending`; use `in_progress` while working and
    /// `completed` when finished.
    #[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum Status {
        /// The task has not been started.
        #[default]
        Pending,
        /// Work on the task is currently underway.
        InProgress,
        /// The task has been finished.
        Completed,
    }

    /// An item in the current session's todo list.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct TodoItem {
        /// The task description shown in the todo list.
        content: String,
        /// The current lifecycle state of the task.
        status: Status,
    }

    /// Todo items retained for the lifetime of one agent session.
    #[state]
    #[derive(Debug, Default)]
    struct TodoList {
        items: Vec<TodoItem>,
    }

    #[tool]
    /// Replace the current session's todo list with the supplied tasks.
    ///
    /// Existing items are discarded. Each supplied task is created with `pending`
    /// status, including when replacing an existing list. Pass an empty array to
    /// create an empty list.
    async fn create_todo_list(
        #[state] state: &mut TodoList,
        #[desc(
            "Tasks to put in the new list, in display order. Replaces the entire existing list; each task starts with `pending` status."
        )]
        content: Vec<String>,
    ) -> Result<()> {
        state.items = content
            .into_iter()
            .map(|content| TodoItem {
                content,
                status: Status::Pending,
            })
            .collect();

        Ok(())
    }

    #[tool]
    /// Change the status of one item in the current session's todo list.
    ///
    /// `index` is zero-based and refers to the item's position in the list.
    /// Returns an error if the index does not exist.
    async fn update(
        #[state] state: &mut TodoList,
        #[desc("Zero-based position of the item to update in the current todo list.")] index: usize,
        #[desc(
            "New lifecycle status for the selected item: `pending`, `in_progress`, or `completed`."
        )]
        status: Status,
    ) -> Result<()> {
        state
            .items
            .get_mut(index)
            .context("todo item index out of range")?
            .status = status;

        Ok(())
    }

    #[tool]
    /// Return every item in the current session's todo list, preserving list order.
    ///
    /// Returns an empty array when no list has been created or after the list is
    /// cleared.
    async fn list(#[state] state: &TodoList) -> Result<Vec<TodoItem>> {
        Ok(state.items.clone())
    }

    #[tool]
    /// Remove every item from the current session's todo list.
    ///
    /// This operation is idempotent and returns an empty list on the next call
    /// to `list`.
    async fn clear(#[state] state: &mut TodoList) -> Result<()> {
        state.items.clear();
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{Status, TodoItem};

        #[test]
        fn todo_list_serializes_as_compact_yaml() {
            let items = vec![
                TodoItem {
                    content: "inspect implementation".into(),
                    status: Status::Completed,
                },
                TodoItem {
                    content: "make the change".into(),
                    status: Status::InProgress,
                },
            ];

            assert_eq!(
                serde_yaml::to_string(&items).unwrap(),
                "- content: inspect implementation\n  status: completed\n- content: make the change\n  status: in_progress\n"
            );
        }

        #[test]
        fn empty_todo_list_serializes_as_yaml_sequence() {
            assert_eq!(
                serde_yaml::to_string(&Vec::<TodoItem>::new()).unwrap(),
                "[]\n"
            );
        }
    }
}

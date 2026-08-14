//! Session-scoped todo-list tools.
//!
//! The extension lets an agent replace a task list, move individual tasks
//! through their lifecycle, inspect current progress, and clear the list.

use e_agent_extension::extension;

#[extension(
    description = "Manage a session-scoped todo list: create or replace the list, update item statuses, inspect all items, and clear the list. In PTC code, statically import the functions, for example `import { create_todo_list, update, list } from \"todo\";`. Calls use positional arguments in the declared order: `create_todo_list([\"inspect\", \"make the change\"])`, `update(0, \"in_progress\")`, `list()`, and `clear()`. Do not use `await import(\"todo\")`, `require(\"todo\")`, or object-shaped arguments such as `update({ index: 0, status: \"completed\" })`. All functions are async; `list()` returns the current array, while `create_todo_list`, `update`, and `clear` return `null`, so do not print or assign their return values.",
    system_prompt = "Use the todo tools proactively when working on complex, non-trivial, or multi-step tasks, when the user explicitly requests a todo list, or when the user provides multiple tasks. Skip them for a single straightforward task, work that takes fewer than three trivial steps, or purely conversational or informational requests. Create a concise list of specific, actionable tasks before starting work, and check the current list first to avoid duplicate tasks. Keep exactly one task in_progress while working unless tasks are being performed in parallel. Mark a task in_progress before starting it and completed immediately after it is fully finished; do not batch status updates or mark work completed while relevant tests or checks are failing. After completing a task, use list to check overall progress and identify the next pending task, and revise the list when new work is discovered or the plan changes. The item index is zero-based, and valid statuses are pending, in_progress, and completed. The list starts empty in each session. In PTC programs, use static top-level imports and positional calls: `import { create_todo_list, update, list } from \"todo\"; await create_todo_list([\"inspect\"]); await update(0, \"in_progress\"); console.log(await list());`. `create_todo_list`, `update`, and `clear` resolve to null; do not log or assign their return values. Call and print `list()` only when a current-state snapshot is needed."
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

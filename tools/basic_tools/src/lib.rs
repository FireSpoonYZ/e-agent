#[path = "bash.rs"]
mod bash_impl;
#[path = "diff.rs"]
mod diff;
#[path = "edit.rs"]
mod edit_impl;
#[path = "fuzzy.rs"]
mod fuzzy;
#[path = "mutation.rs"]
mod mutation;
#[path = "read.rs"]
mod read_impl;
#[path = "write.rs"]
mod write_impl;

use e_agent_tool::{Deserialize, JsonSchema, Result, extension};
use serde_json::Value;

#[derive(Clone, Deserialize, JsonSchema)]
struct Replacement {
    /// Exact text to replace; it must occur exactly once in the original file.
    old_text: String,
    /// Text that replaces old_text.
    new_text: String,
}

#[extension(
    description = "Read, write, edit files and run bash commands in the current workspace",
    system_prompt = "Use basic_tools for file and shell work instead of shelling out to cat, sed, or echo."
)]
mod basic_tools {
    use super::*;

    #[tool]
    /// Read a text file or an image (jpg, png, gif, webp, bmp). Text is limited to 2000 lines or 50KB.
    async fn read(
        #[desc("Path to the file to read, relative to the current directory or absolute")]
        path: String,
        #[desc("1-based line number to start reading from")] offset: Option<usize>,
        #[desc("Maximum number of lines to return")] limit: Option<usize>,
    ) -> Result<Value> {
        read_impl::run(path, offset, limit).await
    }

    #[tool]
    /// Write a UTF-8 text file, creating parent directories and overwriting an existing file.
    async fn write(
        #[desc("Path to the file to write, relative to the current directory or absolute")]
        path: String,
        #[desc("Complete content to write")] content: String,
    ) -> Result<String> {
        write_impl::run(path, content).await
    }

    #[tool]
    /// Edit one text file using exact, unique, non-overlapping replacements matched against the original content. Returns a diff and unified patch.
    async fn edit(
        #[desc("Path to the file to edit, relative to the current directory or absolute")]
        path: String,
        #[desc(
            "One or more exact replacements; each old_text must be unique in the original file"
        )]
        edits: Vec<Replacement>,
    ) -> Result<Value> {
        edit_impl::run(path, edits).await
    }

    #[tool]
    /// Execute a Git Bash command in the current working directory and return stdout and stderr.
    async fn bash(
        #[desc("Bash command to execute")] command: String,
        #[desc("Positive timeout in seconds; omitted means no timeout")] timeout: Option<f64>,
    ) -> Result<String> {
        bash_impl::run(command, timeout).await
    }
}

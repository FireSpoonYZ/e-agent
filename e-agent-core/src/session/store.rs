use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use e_agent_extension::SessionId;
use serde::{Deserialize, Serialize};

use crate::message::Message;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message {
        message: Message,
    },
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        data: serde_json::Value,
    },
}

pub trait SessionStore {
    fn id(&self) -> SessionId;
    fn path(&self) -> &Path;
    fn messages(&self) -> &[Message];
    fn entries(&self) -> &[SessionEntry];
    fn append_message(&mut self, message: Message) -> Result<()>;
    fn append_custom(&mut self, kind: String, data: serde_json::Value) -> Result<()>;
    fn save(&self) -> Result<()>;
}

pub struct JsonlSessionStore {
    id: SessionId,
    path: PathBuf,
    messages: Vec<Message>,
    entries: Vec<SessionEntry>,
}

impl JsonlSessionStore {
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".e/sessions")
                .join(format!("{}.jsonl", uuid::Uuid::new_v4()))
        });
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| uuid::Uuid::parse_str(stem).ok())
            .map(|uuid| SessionId::from_persisted(uuid.as_u128() as u64))
            .unwrap_or_else(SessionId::next);
        let mut store = Self {
            id,
            path,
            messages: Vec::new(),
            entries: Vec::new(),
        };
        if store.path.exists() {
            let file = fs::File::open(&store.path)?;
            for (line_number, line) in BufReader::new(file).lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let entry: SessionEntry = serde_json::from_str(&line).with_context(|| {
                    format!("invalid session entry at line {}", line_number + 1)
                })?;
                if let SessionEntry::Message { message } = &entry {
                    store.messages.push(message.clone());
                }
                store.entries.push(entry);
            }
            let active_goal = store.entries.iter().rev().find_map(|entry| match entry {
                SessionEntry::Custom { custom_type, data } if custom_type == "goal-state" => {
                    Some(data["goal"]["status"] == "active")
                }
                _ => None,
            }) == Some(true);
            if active_goal {
                // Provider custom-call references are process-scoped. Keep durable user and
                // plain assistant context; the resumed run repeats interrupted tool work.
                store.messages.retain(|message| match message {
                    Message::Assistant(message) => !message.content.iter().any(|content| {
                        matches!(content, crate::message::MessageContent::ToolUse { .. })
                    }),
                    Message::ToolResult(_) => false,
                    Message::User(_) => true,
                });
            }
            let answered = store
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::ToolResult(result) => Some(result.tool_use_id.as_str()),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>();
            let dangling = store
                .messages
                .iter()
                .flat_map(Message::tool_uses)
                .filter(|(id, ..)| !answered.contains(id))
                .map(|(id, ..)| id.to_string())
                .collect::<Vec<_>>();
            for id in dangling {
                store.append_message(Message::ToolResult(
                    crate::message::ToolResultMessage::error(
                        id,
                        "tool execution interrupted by process restart",
                    ),
                ))?;
            }
        }
        Ok(store)
    }

    fn append(&mut self, entry: SessionEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &entry)?;
        file.write_all(b"\n")?;
        file.flush()?;
        if let SessionEntry::Message { message } = &entry {
            self.messages.push(message.clone());
        }
        self.entries.push(entry);
        Ok(())
    }
}

impl SessionStore for JsonlSessionStore {
    fn id(&self) -> SessionId {
        self.id
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn messages(&self) -> &[Message] {
        &self.messages
    }
    fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }
    fn append_message(&mut self, message: Message) -> Result<()> {
        self.append(SessionEntry::Message { message })
    }
    fn append_custom(&mut self, kind: String, data: serde_json::Value) -> Result<()> {
        self.append(SessionEntry::Custom {
            custom_type: kind,
            data,
        })
    }
    fn save(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::UserMessage;

    #[test]
    fn creates_unique_uuid_session_paths_and_matching_ids() {
        let first = JsonlSessionStore::open(None).unwrap();
        let second = JsonlSessionStore::open(None).unwrap();
        assert_ne!(first.path(), second.path());
        assert_ne!(first.id(), second.id());
        let uuid =
            uuid::Uuid::parse_str(first.path().file_stem().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(first.id(), SessionId::from_persisted(uuid.as_u128() as u64));

        let restored = JsonlSessionStore::open(Some(first.path().to_owned())).unwrap();
        assert_eq!(restored.id(), first.id());
    }

    #[test]
    fn restores_messages_and_custom_entries() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let mut first = JsonlSessionStore::open(Some(path.clone())).unwrap();
        first
            .append_message(Message::User(UserMessage::text("hello")))
            .unwrap();
        first
            .append_custom("state".into(), serde_json::json!({"n": 1}))
            .unwrap();
        drop(first);
        let restored = JsonlSessionStore::open(Some(path)).unwrap();
        assert_eq!(restored.messages().len(), 1);
        assert_eq!(restored.entries().len(), 2);
    }
}

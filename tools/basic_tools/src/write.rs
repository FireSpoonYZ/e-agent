use std::path::Path;

use e_agent_tool::Result;

use crate::mutation;

pub async fn run(path: String, content: String) -> Result<String> {
    let display_path = path.clone();
    mutation::run(&path, move |absolute| async move {
        if let Some(parent) = Path::new(&absolute)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&absolute, content.as_bytes()).await?;
        Ok(format!(
            "Successfully wrote {} bytes to {display_path}",
            content.len()
        ))
    })
    .await
}

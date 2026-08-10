use std::path::Path;

use e_agent_tool::Result;

pub async fn run(path: String, content: String) -> Result<String> {
    if let Some(parent) = Path::new(&path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(format!(
        "Successfully wrote {} bytes to {path}",
        content.len()
    ))
}

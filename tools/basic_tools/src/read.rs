use e_agent_tool::{Result, anyhow};

const MAX_LINES: usize = 2_000;
const MAX_BYTES: usize = 50 * 1024;

pub async fn run(path: String, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    let offset = offset.unwrap_or(1);
    if offset == 0 {
        return Err(anyhow!("offset must be at least 1"));
    }
    if limit == Some(0) {
        return Err(anyhow!("limit must be at least 1"));
    }

    let bytes = tokio::fs::read(&path).await?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.split('\n').collect();
    if offset > lines.len() {
        return Err(anyhow!(
            "offset {offset} is beyond end of file ({} lines total)",
            lines.len()
        ));
    }

    let requested = limit.unwrap_or(usize::MAX);
    let mut output = String::new();
    let mut shown = 0;
    for line in lines
        .iter()
        .skip(offset - 1)
        .take(requested)
        .take(MAX_LINES)
    {
        let separator = usize::from(shown > 0);
        if output.len() + separator + line.len() > MAX_BYTES {
            break;
        }
        if shown > 0 {
            output.push('\n');
        }
        output.push_str(line);
        shown += 1;
    }

    if shown == 0 && !lines[offset - 1].is_empty() {
        return Ok(format!(
            "[Line {offset} exceeds the {MAX_BYTES} byte output limit. Use bash to inspect it in chunks.]"
        ));
    }

    let next = offset - 1 + shown;
    if next < lines.len() {
        output.push_str(&format!(
            "\n\n[Showing lines {offset}-{next} of {}. Use offset={} to continue.]",
            lines.len(),
            next + 1
        ));
    }
    Ok(output)
}

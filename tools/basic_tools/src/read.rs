use base64::{Engine, engine::general_purpose::STANDARD};
use e_agent_tool::{Result, anyhow};
use serde_json::{Value, json};

const MAX_LINES: usize = 2_000;
const MAX_BYTES: usize = 50 * 1024;

pub async fn run(path: String, offset: Option<usize>, limit: Option<usize>) -> Result<Value> {
    let bytes = tokio::fs::read(&path).await?;
    if let Some(mime_type) = image_mime_type(&bytes) {
        return Ok(json!({
            "type": "image",
            "mime_type": mime_type,
            "data": STANDARD.encode(bytes),
        }));
    }

    let offset = offset.unwrap_or(1);
    if offset == 0 {
        return Err(anyhow!("offset must be at least 1"));
    }
    if limit == Some(0) {
        return Err(anyhow!("limit must be at least 1"));
    }

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
        return Ok(Value::String(format!(
            "[Line {offset} exceeds the {MAX_BYTES} byte output limit. Use bash to inspect it in chunks.]"
        )));
    }

    let next = offset - 1 + shown;
    if next < lines.len() {
        output.push_str(&format!(
            "\n\n[Showing lines {offset}-{next} of {}. Use offset={} to continue.]",
            lines.len(),
            next + 1
        ));
    }
    Ok(Value::String(output))
}

fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::image_mime_type;

    #[test]
    fn detects_supported_images() {
        assert_eq!(image_mime_type(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(image_mime_type(b"GIF89a"), Some("image/gif"));
        assert_eq!(
            image_mime_type(b"RIFF\x00\x00\x00\x00WEBP"),
            Some("image/webp")
        );
        assert_eq!(image_mime_type(b"text"), None);
    }
}

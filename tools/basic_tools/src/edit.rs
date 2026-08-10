use e_agent_tool::{Result, anyhow};

use crate::Replacement;

pub async fn run(path: String, edits: Vec<Replacement>) -> Result<String> {
    if edits.is_empty() {
        return Err(anyhow!("edits must contain at least one replacement"));
    }

    let original = tokio::fs::read_to_string(&path).await?;
    let bom = original.starts_with('\u{feff}');
    let content = original.trim_start_matches('\u{feff}');
    let crlf = content.contains("\r\n");
    let normalized = content.replace("\r\n", "\n");
    let mut ranges = Vec::with_capacity(edits.len());

    for edit in &edits {
        let old = edit.old_text.replace("\r\n", "\n");
        if old.is_empty() {
            return Err(anyhow!("old_text must not be empty"));
        }
        let matches: Vec<_> = normalized
            .match_indices(&old)
            .map(|(index, _)| index)
            .collect();
        if matches.len() != 1 {
            return Err(anyhow!(
                "old_text must occur exactly once in {path}, found {} occurrences",
                matches.len()
            ));
        }
        ranges.push((
            matches[0],
            matches[0] + old.len(),
            edit.new_text.replace("\r\n", "\n"),
        ));
    }

    ranges.sort_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(anyhow!("edits contain overlapping replacements"));
        }
    }

    let mut result = normalized;
    for (start, end, replacement) in ranges.into_iter().rev() {
        result.replace_range(start..end, &replacement);
    }
    if crlf {
        result = result.replace('\n', "\r\n");
    }
    if bom {
        result.insert(0, '\u{feff}');
    }
    tokio::fs::write(&path, result).await?;
    Ok(format!(
        "Successfully replaced {} block(s) in {path}",
        edits.len()
    ))
}

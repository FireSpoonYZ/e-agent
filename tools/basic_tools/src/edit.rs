use e_agent_tool::{Result, anyhow};

use crate::{Replacement, mutation};

pub async fn run(path: String, edits: Vec<Replacement>) -> Result<String> {
    if edits.is_empty() {
        return Err(anyhow!("edits must contain at least one replacement"));
    }

    let display_path = path.clone();
    mutation::run(&path, move |absolute| async move {
        let original = tokio::fs::read_to_string(&absolute).await?;
        let (bom, content) = original
            .strip_prefix('\u{feff}')
            .map_or(("", original.as_str()), |content| ("\u{feff}", content));
        let line_ending = detect_line_ending(content);
        let normalized = normalize(content);
        let mut ranges = Vec::with_capacity(edits.len());

        for edit in &edits {
            let old = normalize(&edit.old_text);
            if old.is_empty() {
                return Err(anyhow!("old_text must not be empty"));
            }
            let matches: Vec<_> = normalized
                .match_indices(&old)
                .map(|(index, _)| index)
                .collect();
            if matches.len() != 1 {
                return Err(anyhow!(
                    "old_text must occur exactly once in {display_path}, found {} occurrences",
                    matches.len()
                ));
            }
            ranges.push((
                matches[0],
                matches[0] + old.len(),
                normalize(&edit.new_text),
            ));
        }

        ranges.sort_by_key(|range| range.0);
        for pair in ranges.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(anyhow!("edits contain overlapping replacements"));
            }
        }

        let mut result = normalized.clone();
        for (start, end, replacement) in ranges.into_iter().rev() {
            result.replace_range(start..end, &replacement);
        }
        if result == normalized {
            return Err(anyhow!(
                "No changes made to {display_path}. The replacements produced identical content."
            ));
        }
        if line_ending == "\r\n" {
            result = result.replace('\n', "\r\n");
        }
        result.insert_str(0, bom);
        tokio::fs::write(&absolute, result).await?;
        Ok(format!(
            "Successfully replaced {} block(s) in {display_path}",
            edits.len()
        ))
    })
    .await
}

fn detect_line_ending(content: &str) -> &'static str {
    match content.find('\n') {
        Some(index) if index > 0 && content.as_bytes()[index - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn normalize(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::Replacement;

    use super::{detect_line_ending, normalize, run};

    #[test]
    fn detects_first_line_ending_and_normalizes_lone_cr() {
        assert_eq!(detect_line_ending("one\r\ntwo\n"), "\r\n");
        assert_eq!(detect_line_ending("one\ntwo\r\n"), "\n");
        assert_eq!(normalize("one\rtwo\r\n"), "one\ntwo\n");
    }

    #[test]
    fn edits_using_the_first_line_ending_style() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "e-agent-edit-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "one\ntarget\r\n").unwrap();
        runtime
            .block_on(run(
                path.to_string_lossy().into_owned(),
                vec![Replacement {
                    old_text: "target".to_string(),
                    new_text: "changed".to_string(),
                }],
            ))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nchanged\n");

        let identical = runtime.block_on(run(
            path.to_string_lossy().into_owned(),
            vec![Replacement {
                old_text: "changed".to_string(),
                new_text: "changed".to_string(),
            }],
        ));
        assert!(
            identical
                .unwrap_err()
                .to_string()
                .contains("No changes made")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nchanged\n");
        std::fs::remove_file(path).unwrap();
    }
}

use e_agent_extension::{Result, anyhow};
use serde_json::json;

use crate::{Replacement, diff, fuzzy, mutation};

pub async fn run(path: String, edits: Vec<Replacement>) -> Result<serde_json::Value> {
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
        let base = normalize(content);
        let (new_content, used_fuzzy) = apply(&base, &edits, &display_path)?;

        let mut written = new_content.clone();
        if line_ending == "\r\n" {
            written = written.replace('\n', "\r\n");
        }
        written.insert_str(0, bom);
        tokio::fs::write(&absolute, written).await?;

        let rendered = diff::render(&base, &new_content, 4);
        Ok(json!({
            "message": format!(
                "Successfully replaced {} block(s) in {display_path}",
                edits.len()
            ),
            "diff": rendered.diff,
            "patch": diff::unified(&display_path, &base, &new_content, 4),
            "first_changed_line": rendered.first_changed_line,
            "used_fuzzy_match": used_fuzzy,
        }))
    })
    .await
}

/// One edit resolved against the search content: offset, length, replacement, index.
type Match<'a> = (usize, usize, &'a str, usize);

/// Match every edit against the original content, then apply them right to left.
///
/// When any edit needs fuzzy matching the whole operation runs in normalized
/// space, and untouched lines are copied back so only edited lines lose their
/// original cosmetic bytes. This mirrors pi's `applyEditsToNormalizedContent`.
fn apply(base: &str, edits: &[Replacement], path: &str) -> Result<(String, bool)> {
    let normalized: Vec<_> = edits
        .iter()
        .map(|edit| (normalize(&edit.old_text), normalize(&edit.new_text)))
        .collect();
    for (index, (old, _)) in normalized.iter().enumerate() {
        if old.is_empty() {
            return Err(anyhow!(
                "edits[{index}].old_text must not be empty in {path}"
            ));
        }
    }

    let used_fuzzy = normalized
        .iter()
        .any(|(old, _)| fuzzy::find(base, old).is_some_and(|(_, _, fuzzy)| fuzzy.is_some()));
    let search = if used_fuzzy {
        fuzzy::normalize(base)
    } else {
        base.to_string()
    };

    let mut matched = Vec::with_capacity(normalized.len());
    for (index, (old, new)) in normalized.iter().enumerate() {
        let (start, length, _) = fuzzy::find(&search, old).ok_or_else(|| {
            anyhow!(
                "Could not find edits[{index}] in {path}. The old_text must match exactly including all whitespace and newlines."
            )
        })?;
        let occurrences = fuzzy::count(&search, old);
        if occurrences > 1 {
            return Err(anyhow!(
                "Found {occurrences} occurrences of edits[{index}] in {path}. Each old_text must be unique."
            ));
        }
        matched.push((start, length, new.as_str(), index));
    }

    matched.sort_by_key(|matched| matched.0);
    for pair in matched.windows(2) {
        if pair[0].0 + pair[0].1 > pair[1].0 {
            return Err(anyhow!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].3,
                pair[1].3
            ));
        }
    }

    let new_content = if used_fuzzy {
        preserve_unchanged_lines(base, &search, &matched)?
    } else {
        replace(&search, &matched)
    };
    if new_content == base {
        return Err(anyhow!(
            "No changes made to {path}. The replacements produced identical content."
        ));
    }
    Ok((new_content, used_fuzzy))
}

fn replace(content: &str, matched: &[Match<'_>]) -> String {
    let mut result = content.to_string();
    for (start, length, new, _) in matched.iter().rev() {
        result.replace_range(start..&(start + length), new);
    }
    result
}

/// Rewrite only the lines a fuzzy replacement touches, copying the rest verbatim.
fn preserve_unchanged_lines(original: &str, base: &str, matched: &[Match<'_>]) -> Result<String> {
    let original_lines = split_lines(original);
    let base_lines = split_lines(base);
    if original_lines.len() != base_lines.len() {
        return Err(anyhow!(
            "cannot preserve unchanged lines because normalization changed the line count"
        ));
    }
    let spans = line_spans(&base_lines);

    let mut groups: Vec<(usize, usize, Vec<&Match<'_>>)> = Vec::new();
    for entry in matched {
        let (start_line, end_line) = line_range(&spans, entry.0, entry.1)?;
        match groups.last_mut() {
            Some(group) if start_line < group.1 => {
                group.1 = group.1.max(end_line);
                group.2.push(entry);
            }
            _ => groups.push((start_line, end_line, vec![entry])),
        }
    }

    let mut result = String::new();
    let mut line = 0;
    for (start_line, end_line, entries) in groups {
        result.push_str(&original_lines[line..start_line].concat());
        let group_start = spans[start_line].0;
        let group_end = spans[end_line - 1].1;
        let shifted: Vec<_> = entries
            .iter()
            .map(|entry| (entry.0 - group_start, entry.1, entry.2, entry.3))
            .collect();
        result.push_str(&replace(&base[group_start..group_end], &shifted));
        line = end_line;
    }
    result.push_str(&original_lines[line..].concat());
    Ok(result)
}

fn split_lines(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let end = rest.find('\n').map_or(rest.len(), |index| index + 1);
        lines.push(&rest[..end]);
        rest = &rest[end..];
    }
    lines
}

fn line_spans(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut offset = 0;
    lines
        .iter()
        .map(|line| {
            let span = (offset, offset + line.len());
            offset = span.1;
            span
        })
        .collect()
}

fn line_range(spans: &[(usize, usize)], start: usize, length: usize) -> Result<(usize, usize)> {
    let end = start + length;
    let start_line = spans
        .iter()
        .position(|span| start >= span.0 && start < span.1)
        .ok_or_else(|| anyhow!("replacement range is outside the file"))?;
    let mut end_line = start_line;
    while end_line < spans.len() && spans[end_line].1 < end {
        end_line += 1;
    }
    if end_line >= spans.len() {
        return Err(anyhow!("replacement range is outside the file"));
    }
    Ok((start_line, end_line + 1))
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

    use super::{apply, detect_line_ending, normalize, run};

    fn edit(old: &str, new: &str) -> Replacement {
        Replacement {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    #[test]
    fn detects_first_line_ending_and_normalizes_lone_cr() {
        assert_eq!(detect_line_ending("one\r\ntwo\n"), "\r\n");
        assert_eq!(detect_line_ending("one\ntwo\r\n"), "\n");
        assert_eq!(normalize("one\rtwo\r\n"), "one\ntwo\n");
    }

    #[test]
    fn falls_back_to_fuzzy_matching_and_keeps_untouched_lines() {
        let base = "let a = \u{201C}x\u{201D};\nlet b = \u{2018}y\u{2019};\n";
        let (content, used_fuzzy) =
            apply(base, &[edit("let a = \"x\";", "let a = 1;")], "f").unwrap();
        assert!(used_fuzzy);
        assert_eq!(content, "let a = 1;\nlet b = \u{2018}y\u{2019};\n");
    }

    #[test]
    fn rejects_missing_duplicate_overlapping_and_identical_edits() {
        let base = "one\ntwo\ntwo\n";
        assert!(apply(base, &[edit("zzz", "x")], "f").is_err());
        assert!(apply(base, &[edit("two", "x")], "f").is_err());
        assert!(apply(base, &[edit("one", "one")], "f").is_err());
        assert!(apply("abcd\n", &[edit("abc", "x"), edit("bcd", "y")], "f").is_err());
        assert!(apply(base, &[edit("", "x")], "f").is_err());
    }

    #[test]
    fn edits_using_the_first_line_ending_style_and_reports_a_diff() {
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
        let result = runtime
            .block_on(run(
                path.to_string_lossy().into_owned(),
                vec![edit("target", "changed")],
            ))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nchanged\n");
        assert_eq!(result["first_changed_line"], 2);
        assert!(result["diff"].as_str().unwrap().contains("-2 target"));
        assert!(result["diff"].as_str().unwrap().contains("+2 changed"));
        assert!(result["patch"].as_str().unwrap().contains("@@"));

        let identical = runtime.block_on(run(
            path.to_string_lossy().into_owned(),
            vec![edit("changed", "changed")],
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

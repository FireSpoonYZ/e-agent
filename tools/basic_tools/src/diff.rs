//! Diff rendering for `edit`, mirroring pi's `generateDiffString` and
//! `generateUnifiedPatch`.

use similar::{ChangeTag, TextDiff};

pub struct Rendered {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

/// Render a line-numbered diff that elides context beyond `context` lines.
pub fn render(old: &str, new: &str, context: usize) -> Rendered {
    let diff = TextDiff::from_lines(old, new);
    let width = old
        .lines()
        .count()
        .max(new.lines().count())
        .max(1)
        .to_string()
        .len();
    let mut output = Vec::new();
    let mut first_changed_line = None;

    for group in diff.grouped_ops(context) {
        if !output.is_empty() {
            output.push(format!(" {:width$} ...", "", width = width));
        }
        for op in group {
            for change in diff.iter_changes(&op) {
                let text = change.value().trim_end_matches('\n');
                match change.tag() {
                    ChangeTag::Insert => {
                        let line = change.new_index().unwrap_or_default() + 1;
                        first_changed_line.get_or_insert(line);
                        output.push(format!("+{line:>width$} {text}", width = width));
                    }
                    ChangeTag::Delete => {
                        let line = change.old_index().unwrap_or_default() + 1;
                        first_changed_line
                            .get_or_insert(change.new_index().unwrap_or(line - 1) + 1);
                        output.push(format!("-{line:>width$} {text}", width = width));
                    }
                    ChangeTag::Equal => {
                        let line = change.old_index().unwrap_or_default() + 1;
                        output.push(format!(" {line:>width$} {text}", width = width));
                    }
                }
            }
        }
    }

    Rendered {
        diff: output.join("\n"),
        first_changed_line,
    }
}

/// Render a standard unified patch with `path` on both headers.
pub fn unified(path: &str, old: &str, new: &str, context: usize) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context)
        .header(path, path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{render, unified};

    #[test]
    fn numbers_changes_and_reports_the_first_changed_line() {
        let rendered = render("one\ntwo\nthree\n", "one\n2\nthree\n", 1);
        assert!(rendered.diff.contains("-2 two"));
        assert!(rendered.diff.contains("+2 2"));
        assert_eq!(rendered.first_changed_line, Some(2));
        assert_eq!(render("same\n", "same\n", 1).first_changed_line, None);
    }

    #[test]
    fn elides_context_between_distant_changes() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new = old.replace("1\n", "one\n").replace("30\n", "thirty\n");
        let rendered = render(&old, &new, 1);
        assert!(rendered.diff.contains("..."));

        let patch = unified("f.txt", &old, &new, 1);
        assert!(patch.contains("--- f.txt"));
        assert!(patch.contains("@@"));
    }
}

// Compare generated output with an existing local file before overwriting it.
// Keep whitespace significant: a build should report any byte change.
pub(super) fn changed(old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    if old.lines().eq(new.lines()) {
        return Some(format!(
            "  - {}\n  + {}",
            if old.ends_with('\n') {
                "final newline"
            } else {
                "no final newline"
            },
            if new.ends_with('\n') {
                "final newline"
            } else {
                "no final newline"
            },
        ));
    }
    Some(lines(old, new))
}

enum Op<'diff> {
    Keep(&'diff str),
    Del(&'diff str),
    Add(&'diff str),
}

// a line diff over an lcs table, rendered as hunks with two context lines;
// scorer sources are tiny so the quadratic table is irrelevant
pub(super) fn lines(old: &str, new: &str) -> String {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();

    let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            ops.push(Op::Keep(old[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(Op::Del(old[i]));
            i += 1;
        } else {
            ops.push(Op::Add(new[j]));
            j += 1;
        }
    }
    ops.extend(old[i..].iter().map(|line| Op::Del(line)));
    ops.extend(new[j..].iter().map(|line| Op::Add(line)));

    // keeps stay only within two lines of a change
    const CONTEXT: usize = 2;
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| (!matches!(op, Op::Keep(_))).then_some(index))
        .collect();
    let visible = |index: usize| changed.iter().any(|&change| index.abs_diff(change) <= CONTEXT);

    let mut rendered = Vec::new();
    let mut elided = false;
    for (index, op) in ops.iter().enumerate() {
        if !visible(index) {
            if !elided {
                rendered.push("  ...".to_owned());
                elided = true;
            }
            continue;
        }
        elided = false;
        rendered.push(match op {
            Op::Keep(line) => format!("    {line}"),
            Op::Del(line) => format!("  - {line}"),
            Op::Add(line) => format!("  + {line}"),
        });
    }

    rendered.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_changed_hunks_with_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh";
        let new = "a\nb\nc\nd changed\ne\nf\ng\nh";
        let diff = lines(old, new);
        assert_eq!(diff, "  ...\n    b\n    c\n  - d\n  + d changed\n    e\n    f\n  ...");
    }

    #[test]
    fn renders_pure_inserts_and_deletes() {
        assert_eq!(lines("a", "a\nb"), "    a\n  + b");
        assert_eq!(lines("a\nb", "b"), "  - a\n    b");
    }

    #[test]
    fn detects_file_changes() {
        assert!(changed("a\n", "a\n").is_none());
        assert!(changed("a  \n", "a\n").is_some());
        assert!(changed("a", "a\n").unwrap().contains("no final newline"));
    }
}

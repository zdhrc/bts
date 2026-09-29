use super::client::RemoteFunction;
use super::component::{Component, Payload};
use crate::dsl::ScorerLang;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

// none = the remote already matches what would be pushed; some = a rendered
// human-readable diff. comparison is field-by-field because remote objects
// carry server fields a whole-object equality would trip over.
pub(super) fn changed(component: &Component, remote: &RemoteFunction) -> Option<String> {
    let mut sections = Vec::new();

    if remote.name != component.name {
        sections.push(format!("  - name: {}\n  + name: {}", remote.name, component.name));
    }

    match &component.payload {
        Payload::Code { lang, code } => {
            let runtime = match lang {
                ScorerLang::Python => "python",
                ScorerLang::Typescript => "node",
            };
            let inline = remote.function_data.get("type").and_then(JsonValue::as_str) == Some("code")
                && remote.function_data.pointer("/data/type").and_then(JsonValue::as_str) == Some("inline")
                && remote
                    .function_data
                    .pointer("/data/runtime_context/runtime")
                    .and_then(JsonValue::as_str)
                    == Some(runtime);
            if inline {
                let remote_code = remote
                    .function_data
                    .pointer("/data/code")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                if !code_equal(remote_code, code) {
                    sections.push(lines(remote_code, code));
                }
            } else {
                // a different shape entirely; put replaces it wholesale
                sections.push(format!("  - {}\n  + an inline {runtime} code scorer", describe(remote)));
            }
        }
        Payload::Prompt {
            model,
            content,
            choice_scores,
            use_cot,
        } => {
            let expected = prompt_subset_local(model, content, choice_scores, *use_cot);
            let found = prompt_subset_remote(remote);
            if found.as_ref() != Some(&expected) {
                let found = found
                    .map(|value| serde_json::to_string_pretty(&value).expect("json maps encode"))
                    .unwrap_or_else(|| describe(remote));
                let expected = serde_json::to_string_pretty(&expected).expect("json maps encode");
                sections.push(lines(&found, &expected));
            }
        }
    }

    (!sections.is_empty()).then(|| sections.join("\n"))
}

fn describe(remote: &RemoteFunction) -> String {
    match remote.function_data.get("type").and_then(JsonValue::as_str) {
        Some(kind) => format!("a {kind} function"),
        None => "a function of unknown shape".to_owned(),
    }
}

// the slice of prompt_data reconciliation cares about, normalized so number
// representation and key order cannot flap the comparison
fn prompt_subset_local(model: &str, content: &str, choice_scores: &[(String, f64)], use_cot: bool) -> JsonValue {
    let scores: JsonMap<String, JsonValue> = choice_scores
        .iter()
        .map(|(label, score)| (label.clone(), json!(score)))
        .collect();
    json!({
        "messages": [{ "role": "user", "content": content }],
        "model": model,
        "use_cot": use_cot,
        "choice_scores": scores,
    })
}

fn prompt_subset_remote(remote: &RemoteFunction) -> Option<JsonValue> {
    if remote.function_data.get("type").and_then(JsonValue::as_str) != Some("prompt") {
        return None;
    }
    let data = remote.prompt_data.as_ref()?;
    let messages: Vec<JsonValue> = data
        .pointer("/prompt/messages")?
        .as_array()?
        .iter()
        .map(|message| {
            json!({
                "role": message.get("role").and_then(JsonValue::as_str).unwrap_or_default(),
                "content": message.get("content").and_then(JsonValue::as_str).unwrap_or_default(),
            })
        })
        .collect();
    let scores: JsonMap<String, JsonValue> = data
        .pointer("/parser/choice_scores")?
        .as_object()?
        .iter()
        .map(|(label, score)| (label.clone(), json!(score.as_f64().unwrap_or(f64::NAN))))
        .collect();
    Some(json!({
        "messages": messages,
        "model": data.pointer("/options/model").and_then(JsonValue::as_str).unwrap_or_default(),
        "use_cot": data.pointer("/parser/use_cot").and_then(JsonValue::as_bool).unwrap_or_default(),
        "choice_scores": scores,
    }))
}

// servers may normalize trailing whitespace; never let that read as a change
fn code_equal(remote: &str, local: &str) -> bool {
    let normalize = |code: &str| {
        code.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    };
    normalize(remote) == normalize(local)
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
    fn normalizes_trailing_whitespace() {
        assert!(code_equal("a  \nb\n\n", "a\nb"));
        assert!(!code_equal("a\nb", "a\nc"));
    }
}

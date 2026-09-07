//! Bounded structural JSON comparison; never persists a response.

use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_COMPARISON_BYTES: usize = 4 * 1024 * 1024;
const MAX_CHANGES: usize = 1_000;
const MAX_VISITED: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone)]
pub struct JsonChange {
    pub pointer: String,
    pub kind: ChangeKind,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

#[derive(Debug, Default)]
pub struct JsonComparison {
    pub changes: Vec<JsonChange>,
    pub truncated: bool,
    pub ignored_subtrees: usize,
    visited: usize,
}

pub fn compare_json(
    before: &[u8],
    after: &[u8],
    ignored: &[String],
) -> Result<JsonComparison, String> {
    if before.len() > MAX_COMPARISON_BYTES || after.len() > MAX_COMPARISON_BYTES {
        return Err("JSON comparison accepts responses up to 4 MiB each.".into());
    }
    for pointer in ignored {
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err(format!(
                "Excluded JSON Pointer must start with '/': {pointer}"
            ));
        }
        let mut chars = pointer.chars();
        while let Some(ch) = chars.next() {
            if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err(format!(
                    "Invalid JSON Pointer escape in {pointer}; use ~0 for ~ and ~1 for /."
                ));
            }
        }
    }
    let before: Value =
        serde_json::from_slice(before).map_err(|e| format!("Baseline is not valid JSON: {e}"))?;
    let after: Value = serde_json::from_slice(after)
        .map_err(|e| format!("Current response is not valid JSON: {e}"))?;
    let mut result = JsonComparison::default();
    visit(Some(&before), Some(&after), "", 0, ignored, &mut result);
    Ok(result)
}

fn visit(
    before: Option<&Value>,
    after: Option<&Value>,
    pointer: &str,
    depth: usize,
    ignored: &[String],
    result: &mut JsonComparison,
) {
    if result.truncated {
        return;
    }
    if result.visited >= MAX_VISITED || result.changes.len() >= MAX_CHANGES || depth > 64 {
        result.truncated = true;
        return;
    }
    result.visited += 1;
    if ignored.iter().any(|item| pointer == item) {
        result.ignored_subtrees += 1;
        return;
    }
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
                let key_pointer =
                    format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                visit(
                    a.get(key),
                    b.get(key),
                    &key_pointer,
                    depth + 1,
                    ignored,
                    result,
                );
                if result.truncated {
                    break;
                }
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for index in 0..a.len().max(b.len()) {
                visit(
                    a.get(index),
                    b.get(index),
                    &format!("{pointer}/{index}"),
                    depth + 1,
                    ignored,
                    result,
                );
                if result.truncated {
                    break;
                }
            }
        }
        (Some(Value::Object(object)), None) | (None, Some(Value::Object(object)))
            if !object.is_empty() =>
        {
            for (key, value) in object {
                let child = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                visit(
                    before.map(|_| value),
                    after.map(|_| value),
                    &child,
                    depth + 1,
                    ignored,
                    result,
                );
                if result.truncated {
                    break;
                }
            }
        }
        (Some(Value::Array(array)), None) | (None, Some(Value::Array(array)))
            if !array.is_empty() =>
        {
            for (index, value) in array.iter().enumerate() {
                visit(
                    before.map(|_| value),
                    after.map(|_| value),
                    &format!("{pointer}/{index}"),
                    depth + 1,
                    ignored,
                    result,
                );
                if result.truncated {
                    break;
                }
            }
        }
        (a, b) if a != b => result.changes.push(JsonChange {
            pointer: pointer.to_owned(),
            kind: match (a, b) {
                (None, _) => ChangeKind::Added,
                (_, None) => ChangeKind::Removed,
                _ => ChangeKind::Changed,
            },
            before: a.cloned(),
            after: b.cloned(),
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_missing_null_and_changes_with_escaped_pointers() {
        let diff = compare_json(
            br#"{"a/b":1,"remove":null,"same":true}"#,
            br#"{"a/b":2,"new":null,"same":true}"#,
            &[],
        )
        .unwrap();
        assert_eq!(diff.changes.len(), 3);
        assert_eq!(diff.changes[0].pointer, "/a~1b");
        assert_eq!(diff.changes[0].kind, ChangeKind::Changed);
        assert_eq!(diff.changes[1].kind, ChangeKind::Added);
        assert_eq!(diff.changes[2].kind, ChangeKind::Removed);
    }

    #[test]
    fn compares_arrays_by_index_and_excludes_only_explicit_subtrees() {
        let diff = compare_json(
            br#"{"items":[1,2],"time":{"now":1}}"#,
            br#"{"items":[1,3,4],"time":{"now":2}}"#,
            &["/time".into()],
        )
        .unwrap();
        assert_eq!(diff.changes.len(), 2);
        assert_eq!(diff.changes[0].pointer, "/items/1");
        assert_eq!(diff.changes[1].kind, ChangeKind::Added);
        assert_eq!(diff.ignored_subtrees, 1);
        assert!(!diff.truncated);
    }

    #[test]
    fn validates_inputs_and_reports_truncation() {
        assert!(compare_json(b"{}", b"html", &[]).is_err());
        assert!(compare_json(b"{}", b"{}", &["/bad~".into()]).is_err());
        assert!(compare_json(&vec![b' '; MAX_COMPARISON_BYTES + 1], b"{}", &[]).is_err());
        let a = serde_json::to_vec(&vec![0; 1100]).unwrap();
        let b = serde_json::to_vec(&vec![1; 1100]).unwrap();
        let diff = compare_json(&a, &b, &[]).unwrap();
        assert!(diff.truncated);
        assert_eq!(diff.changes.len(), MAX_CHANGES);
    }

    #[test]
    fn applies_exclusions_inside_new_and_removed_containers() {
        let before = br#"{}"#;
        let after = br#"{"meta":{"timestamp":1,"name":"order"}}"#;
        for (a, b) in [
            (before.as_slice(), after.as_slice()),
            (after.as_slice(), before.as_slice()),
        ] {
            let diff = compare_json(a, b, &["/meta/timestamp".into()]).unwrap();
            assert_eq!(diff.changes.len(), 1);
            assert_eq!(diff.changes[0].pointer, "/meta/name");
            assert_eq!(diff.ignored_subtrees, 1);
        }
    }
}

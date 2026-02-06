//! Output formatting for lint results.

use crate::checker::DoubleContext;
use crate::unattributed::UnattributedFunction;
use serde::Serialize;

/// JSON-serializable report combining both check types.
#[derive(Debug, Serialize)]
pub struct JsonReport {
    pub double_context: JsonDoubleContextSection,
    pub unattributed: JsonUnattributedSection,
}

#[derive(Debug, Serialize)]
pub struct JsonDoubleContextSection {
    pub warnings: Vec<JsonDoubleContextWarning>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct JsonUnattributedSection {
    pub warnings: Vec<JsonUnattributedWarning>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct JsonDoubleContextWarning {
    pub function_name: String,
    pub call_site: JsonLocation,
    pub definition: JsonLocation,
    pub inner_context: String,
    pub outer_context: Option<String>,
    pub identical: bool,
}

#[derive(Debug, Serialize)]
pub struct JsonUnattributedWarning {
    pub function_name: String,
    pub location: JsonLocation,
    pub is_method: bool,
    pub is_pub: bool,
}

#[derive(Debug, Serialize)]
pub struct JsonLocation {
    pub file: String,
    pub line: usize,
}

// ── Text formatting ─────────────────────────────────────────────────────

/// Format combined results as human-readable text with separate sections.
pub fn format_combined_text(
    double_context: &[DoubleContext],
    unattributed: &[UnattributedFunction],
    strip_prefix: Option<&str>,
) -> String {
    let mut output = String::new();

    if !double_context.is_empty() {
        output.push_str(&format_double_context_text(double_context, strip_prefix));
    }

    if !unattributed.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format_unattributed_text(unattributed, strip_prefix));
    }

    output
}

/// Format double-context results as human-readable text.
fn format_double_context_text(issues: &[DoubleContext], strip_prefix: Option<&str>) -> String {
    let mut output = String::new();

    for issue in issues {
        let call_file = strip_path(&issue.call_file, strip_prefix);
        let def_file = strip_path(&issue.def_file, strip_prefix);

        let outer_display = issue
            .outer_context
            .as_deref()
            .unwrap_or("<complex expression>");

        let method = if issue.is_with_context {
            ".with_context()"
        } else {
            ".context()"
        };

        let identical = is_context_identical(&issue.inner_context, outer_display);

        output.push_str(&format!(
            "warning: double context on `{}`\n",
            issue.function_name
        ));
        output.push_str(&format!("  --> {}:{}\n", call_file, issue.call_line));
        output.push_str(&format!(
            "   | inner context (from #[context]): \"{}\"\n",
            issue.inner_context
        ));
        output.push_str(&format!(
            "   |   defined at: {}:{}\n",
            def_file, issue.def_line
        ));
        output.push_str(&format!(
            "   | outer context (from {method}): \"{outer_display}\"\n",
        ));
        if identical {
            output.push_str("   |\n");
            output.push_str("   = note: these context strings are identical\n");
        }
        output.push('\n');
    }

    output.push_str(&format!(
        "Found {} double-context warning{}\n",
        issues.len(),
        if issues.len() == 1 { "" } else { "s" }
    ));

    output
}

/// Format unattributed function results as human-readable text.
fn format_unattributed_text(issues: &[UnattributedFunction], strip_prefix: Option<&str>) -> String {
    let mut output = String::new();

    for issue in issues {
        let file = strip_path(&issue.file, strip_prefix);

        let vis = if issue.is_pub { "pub " } else { "" };
        let kind = if issue.is_method { "method" } else { "fn" };

        output.push_str(&format!(
            "warning: {kind} returning Result without #[context]: `{}`\n",
            issue.name
        ));
        output.push_str(&format!("  --> {}:{}\n", file, issue.line));
        output.push_str(&format!("   | {vis}{kind} {}\n", issue.name));
        output.push('\n');
    }

    output.push_str(&format!(
        "Found {} unattributed function{} returning anyhow::Result\n",
        issues.len(),
        if issues.len() == 1 { "" } else { "s" }
    ));

    output
}

// ── JSON formatting ─────────────────────────────────────────────────────

/// Format combined results as JSON.
pub fn format_combined_json(
    double_context: &[DoubleContext],
    unattributed: &[UnattributedFunction],
    strip_prefix: Option<&str>,
) -> String {
    let dc_warnings: Vec<JsonDoubleContextWarning> = double_context
        .iter()
        .map(|issue| {
            let outer = issue
                .outer_context
                .as_deref()
                .unwrap_or("<complex expression>");
            JsonDoubleContextWarning {
                function_name: issue.function_name.clone(),
                call_site: JsonLocation {
                    file: strip_path(&issue.call_file, strip_prefix).to_string(),
                    line: issue.call_line,
                },
                definition: JsonLocation {
                    file: strip_path(&issue.def_file, strip_prefix).to_string(),
                    line: issue.def_line,
                },
                inner_context: issue.inner_context.clone(),
                outer_context: issue.outer_context.clone(),
                identical: is_context_identical(&issue.inner_context, outer),
            }
        })
        .collect();

    let ua_warnings: Vec<JsonUnattributedWarning> = unattributed
        .iter()
        .map(|issue| JsonUnattributedWarning {
            function_name: issue.name.clone(),
            location: JsonLocation {
                file: strip_path(&issue.file, strip_prefix).to_string(),
                line: issue.line,
            },
            is_method: issue.is_method,
            is_pub: issue.is_pub,
        })
        .collect();

    let report = JsonReport {
        double_context: JsonDoubleContextSection {
            total: dc_warnings.len(),
            warnings: dc_warnings,
        },
        unattributed: JsonUnattributedSection {
            total: ua_warnings.len(),
            warnings: ua_warnings,
        },
    };

    serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Check if the inner and outer context strings are identical or near-identical.
fn is_context_identical(inner: &str, outer: &str) -> bool {
    // Exact match
    if inner == outer {
        return true;
    }
    // Case-insensitive match
    if inner.eq_ignore_ascii_case(outer) {
        return true;
    }
    false
}

fn strip_path<'a>(path: &'a str, prefix: Option<&str>) -> &'a str {
    match prefix {
        Some(p) => path.strip_prefix(p).unwrap_or(path),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checker::DoubleContext;
    use crate::unattributed::UnattributedFunction;

    fn make_double_context_issue(inner: &str, outer: &str) -> DoubleContext {
        DoubleContext {
            call_file: "/project/src/main.rs".to_string(),
            call_line: 42,
            function_name: "test_fn".to_string(),
            inner_context: inner.to_string(),
            outer_context: Some(outer.to_string()),
            def_file: "/project/src/lib.rs".to_string(),
            def_line: 10,
            is_with_context: false,
        }
    }

    fn make_unattributed_issue(name: &str, is_pub: bool) -> UnattributedFunction {
        UnattributedFunction {
            file: "/project/src/utils.rs".to_string(),
            line: 25,
            name: name.to_string(),
            is_method: false,
            is_pub,
        }
    }

    #[test]
    fn test_double_context_text() {
        let issues = vec![make_double_context_issue(
            "Computing boot digest",
            "Computing boot digest",
        )];
        let output = format_combined_text(&issues, &[], Some("/project/"));
        assert!(output.contains("warning: double context on `test_fn`"));
        assert!(output.contains("src/main.rs:42"));
        assert!(output.contains("these context strings are identical"));
        assert!(output.contains("Found 1 double-context warning"));
    }

    #[test]
    fn test_double_context_different_strings() {
        let issues = vec![make_double_context_issue(
            "Loading config",
            "querying config",
        )];
        let output = format_combined_text(&issues, &[], Some("/project/"));
        assert!(output.contains("warning: double context on `test_fn`"));
        assert!(!output.contains("identical"));
    }

    #[test]
    fn test_unattributed_text() {
        let issues = vec![make_unattributed_issue("find_kernel", false)];
        let output = format_combined_text(&[], &issues, Some("/project/"));
        assert!(output.contains("warning: fn returning Result without #[context]: `find_kernel`"));
        assert!(output.contains("src/utils.rs:25"));
        assert!(output.contains("Found 1 unattributed function"));
    }

    #[test]
    fn test_unattributed_pub() {
        let issues = vec![make_unattributed_issue("public_fn", true)];
        let output = format_combined_text(&[], &issues, Some("/project/"));
        assert!(output.contains("pub fn public_fn"));
    }

    #[test]
    fn test_combined_text() {
        let dc = vec![make_double_context_issue("Loading", "Loading")];
        let ua = vec![make_unattributed_issue("helper", false)];
        let output = format_combined_text(&dc, &ua, Some("/project/"));
        assert!(output.contains("double context"));
        assert!(output.contains("unattributed"));
    }

    #[test]
    fn test_combined_json() {
        let dc = vec![make_double_context_issue("Loading", "Loading")];
        let ua = vec![make_unattributed_issue("helper", false)];
        let output = format_combined_json(&dc, &ua, Some("/project/"));
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["double_context"]["total"], 1);
        assert_eq!(parsed["unattributed"]["total"], 1);
        assert_eq!(parsed["double_context"]["warnings"][0]["identical"], true);
        assert_eq!(
            parsed["unattributed"]["warnings"][0]["function_name"],
            "helper"
        );
    }

    #[test]
    fn test_empty_results() {
        let output = format_combined_text(&[], &[], None);
        assert!(output.is_empty());
    }

    #[test]
    fn test_strip_path() {
        assert_eq!(strip_path("/foo/bar/baz.rs", Some("/foo/")), "bar/baz.rs");
        assert_eq!(strip_path("/foo/bar/baz.rs", None), "/foo/bar/baz.rs");
    }
}

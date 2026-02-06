//! Pass 2: Find call sites where a `#[context]`-annotated function is called
//! and the result is additionally wrapped with `.context()` or `.with_context()`.

use std::path::Path;

use anyhow::{Context, Result};
use syn::visit::Visit;
use syn::{Expr, ExprAwait, ExprCall, ExprMethodCall, ExprPath, File};

use crate::collector::{AnnotatedFunction, AnnotatedFunctions};

/// A detected double-context issue.
#[derive(Debug, Clone)]
pub struct DoubleContext {
    /// File where the call site is located.
    pub call_file: String,
    /// Line number of the `.context()` / `.with_context()` call.
    pub call_line: usize,
    /// The function name that has `#[context]`.
    pub function_name: String,
    /// The context string from the `#[context]` attribute on the function definition.
    pub inner_context: String,
    /// The context string from the `.context()` / `.with_context()` at the call site
    /// (best-effort extraction; may be None if it's a complex expression).
    pub outer_context: Option<String>,
    /// File where the annotated function is defined.
    pub def_file: String,
    /// Line where the annotated function is defined.
    pub def_line: usize,
    /// Whether the outer method was `.with_context()` (vs `.context()`).
    pub is_with_context: bool,
}

/// Information about a callee extracted from a call expression.
#[derive(Debug)]
enum CalleeInfo {
    /// A free function call with path segments.
    /// e.g., `crate::utils::open_dir_remount_rw(args)` -> segments = ["crate", "utils", "open_dir_remount_rw"]
    FreeFunction {
        name: String,
        path_segments: Vec<String>,
    },
    /// A method call on a receiver.
    /// e.g., `imp.prepare()` -> name = "prepare"
    Method { name: String },
}

/// Check a single Rust source file for double-context call sites.
pub fn check_file(path: &Path, index: &AnnotatedFunctions) -> Result<Vec<DoubleContext>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?;

    let syntax: File = match syn::parse_file(&source) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };

    let mut visitor = DoubleContextChecker {
        file_path: path.to_string_lossy().to_string(),
        index,
        results: Vec::new(),
    };
    visitor.visit_file(&syntax);

    Ok(visitor.results)
}

struct DoubleContextChecker<'a> {
    file_path: String,
    index: &'a AnnotatedFunctions,
    results: Vec<DoubleContext>,
}

impl<'a> DoubleContextChecker<'a> {
    /// Given a method call expression for `.context()` or `.with_context()`,
    /// check whether the receiver chain contains a call to an annotated function.
    fn check_context_call(&mut self, method_call: &ExprMethodCall) {
        let method_name = method_call.method.to_string();
        let is_with_context = method_name == "with_context";

        if method_name != "context" && !is_with_context {
            return;
        }

        // Walk the receiver chain to find the underlying function call.
        let callee = match Self::find_callee_in_receiver(&method_call.receiver) {
            Some(c) => c,
            None => return,
        };

        let callee_name = match &callee {
            CalleeInfo::FreeFunction { name, .. } => name,
            CalleeInfo::Method { name } => name,
        };

        // Check if this function name is in our index of annotated functions.
        let annotated_fns = match self.index.get(callee_name) {
            Some(fns) => fns,
            None => return,
        };

        let outer_context = Self::extract_context_arg(method_call);

        // Filter annotated functions to plausible matches based on call type.
        let matches: Vec<&AnnotatedFunction> = annotated_fns
            .iter()
            .filter(|af| Self::is_plausible_match(&callee, af))
            .collect();

        for annotated in matches {
            self.results.push(DoubleContext {
                call_file: self.file_path.clone(),
                call_line: method_call.method.span().start().line,
                function_name: callee_name.clone(),
                inner_context: annotated.context_string.clone(),
                outer_context: outer_context.clone(),
                def_file: annotated.file.clone(),
                def_line: annotated.line,
                is_with_context,
            });
        }
    }

    /// Determine if a callee plausibly matches an annotated function.
    ///
    /// For free function calls with path segments, we require that at least one
    /// non-trivial path segment from the call site appears in the annotated
    /// function's file path. This eliminates most false positives from common
    /// names like `new`, `open`, `parse`, etc.
    ///
    /// For method calls, we require that the annotated function is also a method
    /// (has a `self` receiver).
    fn is_plausible_match(callee: &CalleeInfo, annotated: &AnnotatedFunction) -> bool {
        match callee {
            CalleeInfo::FreeFunction {
                path_segments,
                name,
            } => {
                let common = is_common_function_name(name);

                if path_segments.len() > 1 {
                    // Get qualifying segments (all segments except the last, which is
                    // the function name, and excluding `crate`/`self`/`super`)
                    let qualifying: Vec<&str> = path_segments[..path_segments.len() - 1]
                        .iter()
                        .map(|s| s.as_str())
                        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
                        .collect();

                    if !qualifying.is_empty() {
                        let def_path_lower = annotated.file.to_lowercase();
                        let path_matches = qualifying.iter().any(|seg| {
                            let seg_lower = seg.to_lowercase();
                            def_path_lower.contains(&seg_lower)
                        });

                        if common {
                            // For common names (open, new, copy, etc.), REQUIRE
                            // path match to avoid false positives.
                            return path_matches;
                        }
                        // For distinctive names, path match is nice but not
                        // required — the name itself is strong enough signal.
                    }
                } else if common {
                    // Unqualified call with a common name — too ambiguous.
                    return false;
                }

                // Distinctive name (qualified or not): match by name alone.
                true
            }

            CalleeInfo::Method { name } => {
                // For method calls, only match if the annotated function
                // is also a method (has a `self` receiver).
                // This filters out cases like `hasher.update()` matching
                // a free function `update()` with #[context].
                if annotated.is_method {
                    return true;
                }

                // If the annotated function is NOT a method but has a
                // distinctive name, still consider it — it might be
                // a false positive, but distinctive names are less risky.
                // Actually, if the annotated fn is not a method and the
                // call IS a method call, they can't be the same function.
                // So we should not match.
                //
                // Exception: some functions appear as methods via trait
                // implementations (e.g., FromStr::from_str), and the
                // annotated function might be a free function wrapper.
                // We'll be conservative and skip these to avoid FPs.
                _ = name;
                false
            }
        }
    }

    /// Walk the receiver expression chain to find the underlying function/method call.
    fn find_callee_in_receiver(expr: &Expr) -> Option<CalleeInfo> {
        match expr {
            // Direct function call: `foo(args)` or `module::foo(args)`
            Expr::Call(ExprCall { func, .. }) => Self::extract_callee_from_func(func),

            // `.await` on a function call: `foo(args).await`
            Expr::Await(ExprAwait { base, .. }) => Self::find_callee_in_receiver(base),

            // Method call: `receiver.method(args)` — this is the function we care about
            Expr::MethodCall(inner_method) => Some(CalleeInfo::Method {
                name: inner_method.method.to_string(),
            }),

            // Parenthesized: `(expr)`
            Expr::Paren(paren) => Self::find_callee_in_receiver(&paren.expr),

            // Try expression: `expr?`
            Expr::Try(try_expr) => Self::find_callee_in_receiver(&try_expr.expr),

            _ => None,
        }
    }

    /// Extract callee information from a call expression's function position.
    fn extract_callee_from_func(func: &Expr) -> Option<CalleeInfo> {
        match func {
            Expr::Path(ExprPath { path, .. }) => {
                let segments: Vec<String> = path
                    .segments
                    .iter()
                    .map(|seg| seg.ident.to_string())
                    .collect();
                let name = segments.last()?.clone();
                Some(CalleeInfo::FreeFunction {
                    name,
                    path_segments: segments,
                })
            }
            _ => None,
        }
    }

    /// Try to extract the context string from a `.context("...")` or
    /// `.with_context(|| "...")` call.
    fn extract_context_arg(method_call: &ExprMethodCall) -> Option<String> {
        let first_arg = method_call.args.first()?;

        match first_arg {
            // .context("literal string")
            Expr::Lit(lit) => {
                if let syn::Lit::Str(s) = &lit.lit {
                    Some(s.value())
                } else {
                    None
                }
            }

            // .context(format!("...")) — we can try to extract but it's complex
            Expr::Macro(mac) => {
                let path = &mac.mac.path;
                if path.segments.last().is_some_and(|s| s.ident == "format") {
                    // Best-effort: stringify the macro tokens
                    Some(format!("format!({})", mac.mac.tokens))
                } else {
                    None
                }
            }

            // .with_context(|| "...")
            Expr::Closure(closure) => {
                // Try to extract from the closure body
                match &*closure.body {
                    Expr::Lit(lit) => {
                        if let syn::Lit::Str(s) = &lit.lit {
                            Some(s.value())
                        } else {
                            None
                        }
                    }
                    Expr::Macro(mac) => {
                        let path = &mac.mac.path;
                        if path.segments.last().is_some_and(|s| s.ident == "format") {
                            Some(format!("format!({})", mac.mac.tokens))
                        } else {
                            None
                        }
                    }
                    _ => Some("<complex expression>".to_string()),
                }
            }

            _ => Some("<complex expression>".to_string()),
        }
    }
}

/// Returns true if a function name is so common that matching by name alone
/// (without path qualification) is unreliable.
fn is_common_function_name(name: &str) -> bool {
    matches!(
        name,
        "new"
            | "open"
            | "close"
            | "read"
            | "write"
            | "parse"
            | "from_str"
            | "from"
            | "into"
            | "try_from"
            | "try_into"
            | "default"
            | "clone"
            | "copy"
            | "run"
            | "start"
            | "stop"
            | "init"
            | "create"
            | "delete"
            | "remove"
            | "update"
            | "get"
            | "set"
            | "load"
            | "save"
            | "build"
            | "execute"
            | "exec"
            | "send"
            | "recv"
            | "connect"
            | "bind"
            | "listen"
            | "accept"
            | "flush"
            | "sync"
            | "drop"
            | "status"
            | "display"
            | "fmt"
    )
}

impl<'a, 'ast> Visit<'ast> for DoubleContextChecker<'a> {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.check_context_call(node);
        // Continue visiting child expressions to catch nested cases
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::AnnotatedFunction;
    use std::collections::HashMap;

    fn make_index(entries: Vec<(&str, &str, bool)>) -> AnnotatedFunctions {
        let mut map: AnnotatedFunctions = HashMap::new();
        for (name, ctx, is_method) in entries {
            map.entry(name.to_string())
                .or_default()
                .push(AnnotatedFunction {
                    name: name.to_string(),
                    file: "src/mymodule.rs".to_string(),
                    line: 1,
                    context_string: ctx.to_string(),
                    is_method,
                });
        }
        map
    }

    fn check_source(source: &str, index: &AnnotatedFunctions) -> Vec<DoubleContext> {
        let syntax: File = syn::parse_file(source).unwrap();
        let mut visitor = DoubleContextChecker {
            file_path: "test.rs".to_string(),
            index,
            results: Vec::new(),
        };
        visitor.visit_file(&syntax);
        visitor.results
    }

    #[test]
    fn test_simple_double_context() {
        let index = make_index(vec![("load_config", "Loading config", false)]);
        let results = check_source(
            r#"
            fn main() {
                load_config().context("loading config").unwrap();
            }
            "#,
            &index,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].function_name, "load_config");
        assert_eq!(results[0].outer_context, Some("loading config".to_string()));
    }

    #[test]
    fn test_async_double_context() {
        let index = make_index(vec![("fetch_data", "Fetching data", false)]);
        let results = check_source(
            r#"
            async fn main() {
                fetch_data().await.context("fetching data").unwrap();
            }
            "#,
            &index,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].function_name, "fetch_data");
    }

    #[test]
    fn test_qualified_path() {
        let index = make_index(vec![(
            "get_global_authfile",
            "Loading global authfile",
            false,
        )]);
        let results = check_source(
            r#"
            fn main() {
                ostree_ext::globals::get_global_authfile(&root).context("Querying authfiles").unwrap();
            }
            "#,
            &index,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].function_name, "get_global_authfile");
    }

    #[test]
    fn test_method_call_matches_method() {
        let index = make_index(vec![("prepare", "Preparing import", true)]);
        let results = check_source(
            r#"
            async fn main() {
                imp.prepare().await.context("Init prep derived").unwrap();
            }
            "#,
            &index,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].function_name, "prepare");
    }

    #[test]
    fn test_method_call_does_not_match_free_function() {
        // An annotated free function named "update" should not match
        // a method call `hasher.update()`
        let index = make_index(vec![("update", "Updating test repo", false)]);
        let results = check_source(
            r#"
            fn main() {
                hasher.update(data).context("hashing data").unwrap();
            }
            "#,
            &index,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_common_name_unqualified_filtered() {
        // An unqualified call to a common name like `open()` should be filtered
        let index = make_index(vec![("open", "Opening imgstorage", false)]);
        let results = check_source(
            r#"
            fn main() {
                open(path).context("Opening file").unwrap();
            }
            "#,
            &index,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_common_name_qualified_matching_path() {
        // A qualified call where path segments match the def file should match
        let mut map: AnnotatedFunctions = HashMap::new();
        map.entry("open".to_string())
            .or_default()
            .push(AnnotatedFunction {
                name: "open".to_string(),
                file: "src/podstorage.rs".to_string(),
                line: 284,
                context_string: "Opening imgstorage".to_string(),
                is_method: false,
            });

        let results = check_source(
            r#"
            fn main() {
                podstorage::open(path).context("Opening storage").unwrap();
            }
            "#,
            &map,
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_common_name_qualified_non_matching_path() {
        // A qualified call where path segments DON'T match should not match
        let mut map: AnnotatedFunctions = HashMap::new();
        map.entry("open".to_string())
            .or_default()
            .push(AnnotatedFunction {
                name: "open".to_string(),
                file: "src/podstorage.rs".to_string(),
                line: 284,
                context_string: "Opening imgstorage".to_string(),
                is_method: false,
            });

        let results = check_source(
            r#"
            fn main() {
                std::fs::File::open(path).context("Opening file").unwrap();
            }
            "#,
            &map,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_with_context() {
        let index = make_index(vec![(
            "inspect_filesystem",
            "Inspecting filesystem {path}",
            false,
        )]);
        let results = check_source(
            r#"
            fn main() {
                inspect_filesystem(&path).with_context(|| format!("Inspecting /boot")).unwrap();
            }
            "#,
            &index,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].is_with_context);
    }

    #[test]
    fn test_no_double_context() {
        let index = make_index(vec![("load_config", "Loading config", false)]);
        let results = check_source(
            r#"
            fn main() {
                // No .context() call — this is fine
                load_config().unwrap();
            }
            "#,
            &index,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_unrelated_context_call() {
        let index = make_index(vec![("load_config", "Loading config", false)]);
        let results = check_source(
            r#"
            fn main() {
                // .context() on a different function — should not match
                something_else().context("whatever").unwrap();
            }
            "#,
            &index,
        );
        assert!(results.is_empty());
    }
}

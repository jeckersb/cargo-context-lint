//! Pass 1: Collect all functions annotated with `#[context(...)]` from `fn_error_context`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use syn::visit::Visit;
use syn::{Attribute, File, ImplItemFn, ItemFn, TraitItemFn};

/// Information about a function annotated with `#[context("...")]`.
#[derive(Debug, Clone)]
pub struct AnnotatedFunction {
    /// The function name.
    pub name: String,
    /// The file path where the function is defined.
    pub file: String,
    /// The line number of the function definition.
    pub line: usize,
    /// The context string from the `#[context("...")]` attribute.
    pub context_string: String,
    /// Whether this is a method (has a `self` receiver).
    pub is_method: bool,
}

/// A map from function name to all annotated functions with that name.
/// Multiple functions can share a name (different modules/impls).
pub type AnnotatedFunctions = HashMap<String, Vec<AnnotatedFunction>>;

/// Parse a single Rust source file and collect all `#[context(...)]`-annotated functions.
pub fn collect_from_file(path: &Path) -> Result<Vec<AnnotatedFunction>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?;

    let syntax: File = match syn::parse_file(&source) {
        Ok(f) => f,
        Err(_) => {
            // Some files may not parse (e.g., macro-heavy code). Skip them.
            return Ok(Vec::new());
        }
    };

    let mut visitor = ContextCollector {
        file_path: path.to_string_lossy().to_string(),
        results: Vec::new(),
    };
    visitor.visit_file(&syntax);

    Ok(visitor.results)
}

/// Build the full map of annotated functions from a list of collected entries.
pub fn build_index(entries: Vec<AnnotatedFunction>) -> AnnotatedFunctions {
    let mut map: AnnotatedFunctions = HashMap::new();
    for entry in entries {
        map.entry(entry.name.clone()).or_default().push(entry);
    }
    map
}

/// AST visitor that collects functions with `#[context(...)]` attributes.
struct ContextCollector {
    file_path: String,
    results: Vec<AnnotatedFunction>,
}

impl ContextCollector {
    /// Check if an attribute is a `#[context(...)]` or `#[fn_error_context::context(...)]`
    /// attribute, and if so, extract the context string.
    fn extract_context_string(attr: &Attribute) -> Option<String> {
        let path = attr.path();

        let is_context = match path.segments.len() {
            // `#[context("...")]` — requires a `use fn_error_context::context;` import
            1 => path.segments[0].ident == "context",
            // `#[fn_error_context::context("...")]`
            2 => {
                path.segments[0].ident == "fn_error_context" && path.segments[1].ident == "context"
            }
            _ => false,
        };

        if !is_context {
            return None;
        }

        // Extract the context string from the attribute arguments.
        // The attribute takes the form: #[context("format string", args...)]
        // or #[context(move, "format string", args...)]
        // We want the first string literal.
        let tokens = match &attr.meta {
            syn::Meta::List(list) => list.tokens.clone(),
            _ => return None,
        };

        // Find the first string literal in the token stream.
        for token in tokens {
            if let proc_macro2::TokenTree::Literal(lit) = token {
                let repr = lit.to_string();
                // String literals start and end with '"'
                if repr.starts_with('"') && repr.ends_with('"') {
                    // Strip the surrounding quotes
                    return Some(repr[1..repr.len() - 1].to_string());
                }
            }
        }

        None
    }

    fn check_fn(
        &mut self,
        attrs: &[Attribute],
        name: &str,
        is_method: bool,
        span_start: proc_macro2::Span,
    ) {
        for attr in attrs {
            if let Some(context_string) = Self::extract_context_string(attr) {
                self.results.push(AnnotatedFunction {
                    name: name.to_string(),
                    file: self.file_path.clone(),
                    line: span_start.start().line,
                    context_string,
                    is_method,
                });
                break; // Only one #[context] per function
            }
        }
    }
}

impl<'ast> Visit<'ast> for ContextCollector {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.check_fn(
            &node.attrs,
            &node.sig.ident.to_string(),
            node.sig.receiver().is_some(),
            node.sig.ident.span(),
        );
        // Continue visiting nested items
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.check_fn(
            &node.attrs,
            &node.sig.ident.to_string(),
            node.sig.receiver().is_some(),
            node.sig.ident.span(),
        );
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        self.check_fn(
            &node.attrs,
            &node.sig.ident.to_string(),
            node.sig.receiver().is_some(),
            node.sig.ident.span(),
        );
        syn::visit::visit_trait_item_fn(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_collect(source: &str) -> Vec<AnnotatedFunction> {
        let syntax: File = syn::parse_file(source).unwrap();
        let mut visitor = ContextCollector {
            file_path: "test.rs".to_string(),
            results: Vec::new(),
        };
        visitor.visit_file(&syntax);
        visitor.results
    }

    #[test]
    fn test_simple_context() {
        let results = parse_and_collect(
            r#"
            use fn_error_context::context;

            #[context("Loading config")]
            fn load_config() -> Result<()> {
                Ok(())
            }
        "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "load_config");
        assert_eq!(results[0].context_string, "Loading config");
        assert!(!results[0].is_method);
    }

    #[test]
    fn test_fully_qualified() {
        let results = parse_and_collect(
            r#"
            #[fn_error_context::context("Deleting entry")]
            fn delete_entry() -> Result<()> {
                Ok(())
            }
        "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "delete_entry");
        assert_eq!(results[0].context_string, "Deleting entry");
    }

    #[test]
    fn test_format_args() {
        let results = parse_and_collect(
            r#"
            use fn_error_context::context;

            #[context("Opening {target} with writable mount")]
            fn open_dir_remount_rw(target: &str) -> Result<()> {
                Ok(())
            }
        "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].context_string,
            "Opening {target} with writable mount"
        );
    }

    #[test]
    fn test_method() {
        let results = parse_and_collect(
            r#"
            use fn_error_context::context;

            struct Foo;
            impl Foo {
                #[context("Preparing import")]
                async fn prepare(&mut self) -> Result<()> {
                    Ok(())
                }
            }
        "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "prepare");
        assert!(results[0].is_method);
    }

    #[test]
    fn test_no_context() {
        let results = parse_and_collect(
            r#"
            fn no_annotation() -> Result<()> {
                Ok(())
            }
        "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_positional_format_args() {
        let results = parse_and_collect(
            r#"
            #[fn_error_context::context("Deleting {}", entry.name)]
            fn delete(entry: &Entry) -> Result<()> {
                Ok(())
            }
        "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].context_string, "Deleting {}");
    }
}

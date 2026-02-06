//! Check for functions returning `anyhow::Result` without a `#[context]` attribute.
//!
//! Functions that return `anyhow::Result` should generally have a `#[context("...")]`
//! annotation from the `fn_error_context` crate to provide meaningful error context.
//! This module detects functions that are missing this annotation.

use std::path::Path;

use anyhow::{Context, Result};
use syn::visit::Visit;
use syn::{
    Attribute, File, GenericArgument, ImplItemFn, ItemFn, ItemImpl, ItemMod, PathArguments,
    ReturnType, Signature, Type, Visibility,
};

/// A function returning `anyhow::Result` without `#[context]`.
#[derive(Debug, Clone)]
pub struct UnattributedFunction {
    /// File where the function is defined.
    pub file: String,
    /// Line number of the function definition.
    pub line: usize,
    /// The function name.
    pub name: String,
    /// Whether this is a method (has a `self` receiver).
    pub is_method: bool,
    /// Whether this function has `pub` visibility.
    pub is_pub: bool,
}

/// Check a single Rust source file for functions returning `anyhow::Result`
/// without a `#[context]` attribute.
pub fn check_file(path: &Path) -> Result<Vec<UnattributedFunction>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?;

    let syntax: File = match syn::parse_file(&source) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };

    // Determine if `anyhow::Result` is in scope at the file level.
    let has_anyhow_result_import = has_anyhow_result_in_scope(&syntax);

    // Check for non-anyhow `type Result` aliases that shadow the import.
    let has_non_anyhow_result_alias = has_non_anyhow_result_alias(&syntax);

    let mut visitor = UnattributedChecker {
        file_path: path.to_string_lossy().to_string(),
        anyhow_result_imported: has_anyhow_result_import && !has_non_anyhow_result_alias,
        in_cfg_test: false,
        in_trait_impl: false,
        results: Vec::new(),
    };
    visitor.visit_file(&syntax);

    Ok(visitor.results)
}

/// Check if the file has `use anyhow::Result` or equivalent in scope.
fn has_anyhow_result_in_scope(file: &File) -> bool {
    for item in &file.items {
        if let syn::Item::Use(use_item) = item {
            if use_tree_imports_anyhow_result(&use_item.tree) {
                return true;
            }
        }
    }
    false
}

/// Recursively check a use tree for `anyhow::Result`.
fn use_tree_imports_anyhow_result(tree: &syn::UseTree) -> bool {
    match tree {
        // `use anyhow::Result;`
        syn::UseTree::Path(path) => {
            if path.ident == "anyhow" {
                return use_subtree_imports_result(&path.tree);
            }
            false
        }
        // `use anyhow::*;` at top level won't have "anyhow" as the ident here,
        // but we handle it via the Path case above.
        _ => false,
    }
}

/// Check if a subtree under `anyhow::` imports `Result`.
fn use_subtree_imports_result(tree: &syn::UseTree) -> bool {
    match tree {
        // `use anyhow::Result;`
        syn::UseTree::Name(name) => name.ident == "Result",
        // `use anyhow::Result as AnyhowResult;`
        syn::UseTree::Rename(rename) => rename.ident == "Result",
        // `use anyhow::*;`
        syn::UseTree::Glob(_) => true,
        // `use anyhow::{Result, Context};`
        syn::UseTree::Group(group) => group.items.iter().any(use_subtree_imports_result),
        // `use anyhow::something::Result;`
        syn::UseTree::Path(_) => false,
    }
}

/// Check if the file has a `type Result<T> = ...;` alias that is NOT `anyhow::Result`.
fn has_non_anyhow_result_alias(file: &File) -> bool {
    for item in &file.items {
        if let syn::Item::Type(type_alias) = item {
            if type_alias.ident == "Result" {
                // Check if the RHS is `anyhow::Result<T>` — if so, it's fine.
                // If it's something else, it shadows the import.
                if !is_anyhow_result_type(&type_alias.ty) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a type is `anyhow::Result<T>`.
fn is_anyhow_result_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let segments: Vec<String> = type_path
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        // `anyhow::Result` or just `Result` (which would be the imported one)
        if segments == ["anyhow", "Result"] || segments == ["Result"] {
            return true;
        }
    }
    false
}

struct UnattributedChecker {
    file_path: String,
    /// Whether `anyhow::Result` is imported at the file level.
    anyhow_result_imported: bool,
    /// Whether we are inside a `#[cfg(test)]` module.
    in_cfg_test: bool,
    /// Whether we are inside a trait impl block (`impl Trait for Type`).
    in_trait_impl: bool,
    results: Vec<UnattributedFunction>,
}

impl UnattributedChecker {
    /// Check a function signature and attributes to decide if it should be flagged.
    fn check_fn(&mut self, attrs: &[Attribute], sig: &Signature, vis: Option<&Visibility>) {
        // Skip if inside a #[cfg(test)] module
        if self.in_cfg_test {
            return;
        }

        // Skip if inside a trait impl block
        if self.in_trait_impl {
            return;
        }

        // Skip if named `main`
        if sig.ident == "main" {
            return;
        }

        // Skip if has #[test] attribute
        if has_test_attribute(attrs) {
            return;
        }

        // Skip if already has #[context] attribute
        if has_context_attribute(attrs) {
            return;
        }

        // Check if the return type looks like `anyhow::Result<T>`
        if !self.returns_anyhow_result(sig) {
            return;
        }

        let is_pub = matches!(vis, Some(Visibility::Public(_)));

        self.results.push(UnattributedFunction {
            file: self.file_path.clone(),
            line: sig.ident.span().start().line,
            name: sig.ident.to_string(),
            is_method: sig.receiver().is_some(),
            is_pub,
        });
    }

    /// Check if a function signature returns what looks like `anyhow::Result<T>`.
    fn returns_anyhow_result(&self, sig: &Signature) -> bool {
        let return_type = match &sig.output {
            ReturnType::Default => return false,
            ReturnType::Type(_, ty) => ty.as_ref(),
        };

        match return_type {
            Type::Path(type_path) => {
                let segments: Vec<String> = type_path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();

                // Explicitly qualified: `anyhow::Result<T>`
                if segments == ["anyhow", "Result"] {
                    return true;
                }

                // Bare `Result<T>` — only if anyhow::Result is imported
                if segments == ["Result"] && self.anyhow_result_imported {
                    // Make sure it has exactly one type argument (not `Result<T, E>`)
                    if let Some(last_seg) = type_path.path.segments.last() {
                        return has_single_type_argument(&last_seg.arguments);
                    }
                }

                false
            }
            _ => false,
        }
    }
}

/// Check if a `#[cfg(test)]` attribute is present.
fn has_cfg_test_attribute(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("cfg") {
            continue;
        }
        // Check if the argument is `test`
        if let syn::Meta::List(list) = &attr.meta {
            let tokens_str = list.tokens.to_string();
            if tokens_str.trim() == "test" {
                return true;
            }
        }
    }
    false
}

/// Check if a `#[test]` attribute is present.
fn has_test_attribute(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        // #[test]
        (path.segments.len() == 1 && path.segments[0].ident == "test")
        // #[tokio::test] or similar
        || (path.segments.len() == 2 && path.segments[1].ident == "test")
    })
}

/// Check if a `#[context]` or `#[fn_error_context::context]` attribute is present.
fn has_context_attribute(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        match path.segments.len() {
            1 => path.segments[0].ident == "context",
            2 => {
                path.segments[0].ident == "fn_error_context" && path.segments[1].ident == "context"
            }
            _ => false,
        }
    })
}

/// Check if path arguments contain exactly one type argument.
/// This distinguishes `Result<T>` (anyhow) from `Result<T, E>` (std).
fn has_single_type_argument(args: &PathArguments) -> bool {
    match args {
        PathArguments::AngleBracketed(angle) => {
            let type_args: Vec<_> = angle
                .args
                .iter()
                .filter(|arg| matches!(arg, GenericArgument::Type(_)))
                .collect();
            type_args.len() == 1
        }
        _ => false,
    }
}

impl<'ast> Visit<'ast> for UnattributedChecker {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.check_fn(&node.attrs, &node.sig, Some(&node.vis));
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.check_fn(&node.attrs, &node.sig, Some(&node.vis));
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let prev_in_trait_impl = self.in_trait_impl;

        // If this is `impl Trait for Type`, set the flag
        if node.trait_.is_some() {
            self.in_trait_impl = true;
        }

        syn::visit::visit_item_impl(self, node);

        self.in_trait_impl = prev_in_trait_impl;
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let prev_in_cfg_test = self.in_cfg_test;

        // If this module has #[cfg(test)], set the flag
        if has_cfg_test_attribute(&node.attrs) {
            self.in_cfg_test = true;
        }

        syn::visit::visit_item_mod(self, node);

        self.in_cfg_test = prev_in_cfg_test;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_source(source: &str) -> Vec<UnattributedFunction> {
        let syntax: File = syn::parse_file(source).unwrap();
        let has_import = has_anyhow_result_in_scope(&syntax);
        let has_alias = has_non_anyhow_result_alias(&syntax);

        let mut visitor = UnattributedChecker {
            file_path: "test.rs".to_string(),
            anyhow_result_imported: has_import && !has_alias,
            in_cfg_test: false,
            in_trait_impl: false,
            results: Vec::new(),
        };
        visitor.visit_file(&syntax);
        visitor.results
    }

    #[test]
    fn test_flagged_without_context() {
        let results = check_source(
            r#"
            use anyhow::Result;

            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "do_something");
    }

    #[test]
    fn test_not_flagged_with_context() {
        let results = check_source(
            r#"
            use anyhow::Result;
            use fn_error_context::context;

            #[context("Doing something")]
            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_test_fn() {
        let results = check_source(
            r#"
            use anyhow::Result;

            #[test]
            fn test_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_tokio_test() {
        let results = check_source(
            r#"
            use anyhow::Result;

            #[tokio::test]
            async fn test_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_cfg_test_module() {
        let results = check_source(
            r#"
            use anyhow::Result;

            #[cfg(test)]
            mod tests {
                use super::*;

                fn helper() -> Result<()> {
                    Ok(())
                }

                #[test]
                fn test_something() -> Result<()> {
                    helper()
                }
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_main() {
        let results = check_source(
            r#"
            use anyhow::Result;

            fn main() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_trait_impl() {
        let results = check_source(
            r#"
            use anyhow::Result;

            struct Foo;

            impl std::str::FromStr for Foo {
                type Err = anyhow::Error;
                fn from_str(s: &str) -> Result<Self> {
                    Ok(Foo)
                }
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_two_type_params() {
        let results = check_source(
            r#"
            use anyhow::Result;

            fn do_something() -> Result<(), std::io::Error> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_io_result() {
        let results = check_source(
            r#"
            fn do_something() -> std::io::Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_flagged_explicit_anyhow_result() {
        let results = check_source(
            r#"
            fn do_something() -> anyhow::Result<()> {
                Ok(())
            }
            "#,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "do_something");
    }

    #[test]
    fn test_not_flagged_no_import() {
        let results = check_source(
            r#"
            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_flagged_with_use_group() {
        let results = check_source(
            r#"
            use anyhow::{Context, Result};

            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_flagged_with_glob_import() {
        let results = check_source(
            r#"
            use anyhow::*;

            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_not_flagged_non_anyhow_alias() {
        let results = check_source(
            r#"
            use anyhow::Result;

            type Result<T> = std::result::Result<T, MyError>;

            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_not_flagged_no_return_type() {
        let results = check_source(
            r#"
            use anyhow::Result;

            fn do_something() {
            }
            "#,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_flagged_method_in_inherent_impl() {
        let results = check_source(
            r#"
            use anyhow::Result;

            struct Foo;

            impl Foo {
                fn do_something(&self) -> Result<()> {
                    Ok(())
                }
            }
            "#,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].is_method);
    }

    #[test]
    fn test_pub_visibility_tracked() {
        let results = check_source(
            r#"
            use anyhow::Result;

            pub fn public_fn() -> Result<()> {
                Ok(())
            }

            fn private_fn() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|r| r.name == "public_fn" && r.is_pub));
        assert!(results.iter().any(|r| r.name == "private_fn" && !r.is_pub));
    }

    #[test]
    fn test_not_flagged_fully_qualified_context() {
        let results = check_source(
            r#"
            use anyhow::Result;

            #[fn_error_context::context("Doing something")]
            fn do_something() -> Result<()> {
                Ok(())
            }
            "#,
        );
        assert!(results.is_empty());
    }
}

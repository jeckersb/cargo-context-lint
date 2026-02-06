# cargo-context-lint

A cargo plugin that detects error context issues in Rust projects using
the [`fn_error_context`](https://crates.io/crates/fn_error_context) and
[`anyhow`](https://crates.io/crates/anyhow) crates.

## Problem

The `fn_error_context` crate provides a `#[context("...")]` attribute macro
that automatically wraps a function's body to add context to any returned
error. A common mistake is to then *also* add `.context()` or
`.with_context()` at the call site, producing redundant nested error
messages:

```rust
#[context("Loading config")]
fn load_config() -> Result<Config> { /* ... */ }

// Bad: produces "Loading config: Loading config: <actual error>"
let cfg = load_config().context("Loading config")?;

// Good: the #[context] on load_config() already provides context
let cfg = load_config()?;
```

A related issue is functions returning `anyhow::Result` that lack a
`#[context]` annotation entirely, meaning errors propagated through them
carry no descriptive context.

## Checks

### Double context (always enabled)

Finds call sites where a function annotated with `#[context("...")]` is
called and the result is additionally wrapped with `.context()` or
`.with_context()`.

### Unattributed functions (`--unattributed`, default: `deny`)

Finds functions returning `anyhow::Result` that lack a `#[context]`
annotation. The following are excluded:

- `#[test]` functions and `#[tokio::test]` functions
- Functions inside `#[cfg(test)]` modules
- `main()` functions
- Trait implementation methods (`impl Trait for Type`)
- Functions returning `Result<T, E>` with an explicit error type
- Functions in files that don't import `anyhow::Result`

## Installation

```sh
cargo install --path .
```

Or from a git repository:

```sh
cargo install --git https://github.com/OWNER/cargo-context-lint
```

## Usage

```sh
# Run both checks in the current workspace
cargo context-lint

# Only run the double-context check
cargo context-lint --unattributed allow

# JSON output (for CI/tooling integration)
cargo context-lint --format json

# Check a specific workspace
cargo context-lint --manifest-path /path/to/Cargo.toml

# Show all annotated functions found during analysis
cargo context-lint --verbose
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0    | No issues found |
| 1    | Issues were found |
| 2    | Tool error (e.g., failed to parse Cargo.toml) |

## Limitations

- **Name-based matching**: The tool uses syntactic analysis (`syn`) without
  type resolution. Function calls are matched to annotated definitions by
  name. For common names like `new`, `open`, `copy`, etc., the tool
  requires qualifying path segments to match, but false positives from name
  collisions are possible in rare cases.

- **Workspace-only**: Only source files within the current cargo workspace
  are analyzed. `#[context]`-annotated functions in external dependencies
  are not detected.

- **Macro-generated code**: Function definitions or calls inside macro
  invocations may not be visible to the parser.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option.

# Skill: Write Unit Tests

Use this skill when adding or updating unit tests in this repository.

## Purpose

Add unit tests while adhering to the repository's strict file organization rules. Production files should remain focused on production code.

## Inspect First

- `docs/operations/test-writing.md`
- `docs/operations/testing.md`

## Workflow

1. Determine the module structure
    - For a module `foo.rs`, tests live in a dedicated sibling directory `foo/`.
    - Create `foo/tests.rs` to declare submodules and shared helpers.
    - Create thematically grouped submodules (e.g., `foo/tests/happy_path.rs`).
    - For crate roots (`lib.rs` / `main.rs`), tests live in a sibling `tests/` directory and `tests.rs` file.

2. Wiring up the tests
    - Add `#[cfg(test)] mod tests;` to the end of the production module file.
    - Inside `foo/tests.rs`, declare the submodules (e.g., `mod happy_path;`).
    - Inside individual test files (e.g., `foo/tests/happy_path.rs`), use `use super::super::*;` to access production items, or `use super::*;` to access shared helpers in `tests.rs`.

3. Ensure correct layout
    - Do NOT write inline test modules (i.e. `#[cfg(test)] mod tests { ... }`).
    - Every file containing tests must be within a test folder (e.g., `foo/tests/basics.rs`), even if it is the only test file.
    - Use `cargo test -p <crate>` to verify changes.

## Repository-Specific Rules

- **`pim-daemon` exception:** Tests for modules loaded via `#[path = "X.rs"] mod X;` in `pim-daemon/src/app.rs` need special handling. See `docs/operations/test-writing.md` for explicit `#[path]` attribute requirements on the test module declarations.
- No `tests/mod.rs`. Use the Rust 2018+ idiom of `tests.rs` + `tests/` directory.

## Done Criteria

- Tests run successfully (`cargo test --workspace`).
- Build warnings are eliminated (`cargo build --workspace --tests`).
- No inline test modules exist (`grep -rn '^mod tests {' crates/` returns nothing).

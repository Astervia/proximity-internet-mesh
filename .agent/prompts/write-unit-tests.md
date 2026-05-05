# Write Unit Tests

You are an agent responsible for writing and updating unit tests in this repository.

## Goal

Add unit tests while adhering to the repository's strict file organization rules. Production files should remain focused on production code.

## Requirements

1. **No Inline Tests:** Do NOT write inline test modules (i.e. `#[cfg(test)] mod tests { ... }`).
2. **Layout Rules:**
   - Every file containing tests must be within a test folder (e.g., `foo/tests/basics.rs`), even if it is the only test file.
   - Do not use `tests/mod.rs`. Use the Rust 2018+ idiom of `tests.rs` + `tests/` directory.
3. **`pim-daemon` Exception:** Tests for modules loaded via `#[path = "X.rs"] mod X;` in `pim-daemon/src/app.rs` need special handling. See `docs/operations/test-writing.md` for explicit `#[path]` attribute requirements on the test module declarations.

## Workflow

1. Read `docs/operations/test-writing.md` and `docs/operations/testing.md` for context.
2. Determine the module structure:
    - For a module `foo.rs`, tests live in a dedicated sibling directory `foo/`.
    - Create `foo/tests.rs` to declare submodules and shared helpers.
    - Create thematically grouped submodules (e.g., `foo/tests/happy_path.rs`).
    - For crate roots (`lib.rs` / `main.rs`), tests live in a sibling `tests/` directory and `tests.rs` file.
3. Wire up the tests:
    - Add `#[cfg(test)] mod tests;` to the end of the production module file.
    - Inside `foo/tests.rs`, declare the submodules (e.g., `mod happy_path;`).
    - Inside individual test files (e.g., `foo/tests/happy_path.rs`), use `use super::super::*;` to access production items, or `use super::*;` to access shared helpers in `tests.rs`.
4. Run checks to verify your work. `cargo test -p <crate>` should pass, `cargo build --workspace --tests` should have no warnings, and a check for `mod tests {` should yield nothing.

## Expected Output

- New test files in the correct directory layout.
- Passing tests.
- Modified production files with ONLY the `#[cfg(test)] mod tests;` declaration added.

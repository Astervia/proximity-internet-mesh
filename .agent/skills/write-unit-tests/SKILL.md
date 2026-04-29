# Skill: Write Unit Tests

Use this skill when adding or updating unit tests in this repository.

## Purpose

Standardize how unit tests are organized to ensure consistency and maintainability, specifically avoiding the inline `#[cfg(test)] mod tests { ... }` pattern that agents often default to.

## Core Rules

1. **No Inline Test Modules:** Never write `#[cfg(test)] mod tests { ... }` inline within a production file.
2. **Dedicated Folder:** Tests belong in a separate directory named after the module file (e.g., `foo.rs` and `foo/tests/`).
3. **Module Wiring:**
   - `foo.rs` ends with:
     ```rust
     #[cfg(test)]
     mod tests;
     ```
   - `foo/tests.rs` declares test themes:
     ```rust
     mod basics;
     ```
   - `foo/tests/basics.rs` contains the actual tests, importing parent code with `use super::super::*;`.

## Detailed Guidance

Always refer to `docs/operations/test-writing.md` for the comprehensive layout rules before writing tests. This covers specifics like how to handle crate roots (`lib.rs` / `main.rs`) and exceptions like `pim-daemon`'s `#[path]` module loading.

## Verification

After writing tests, verify your changes by ensuring there are no inline test blocks and that the tests compile and pass:

1. `grep -rn '^mod tests {' crates/` (should return no results)
2. `cargo test --workspace`

# Skill: Update Dependencies

Use this skill when updating dependencies in the `Cargo.toml` file or handling dependabot PRs.

## Purpose

Standardize the workflow for bumping dependencies to ensure the workspace remains healthy, builds correctly, and does not introduce regressions against established project conventions.

## Workflow

1. Identify the dependency to update.
    - Check recent dependabot PRs or security alerts.
    - Review the workspace `Cargo.toml`.

2. Update the version in `Cargo.toml`.
    - If the dependency is a workspace dependency, update it in the root `Cargo.toml`.
    - If it's a crate-specific dependency, update it in the respective crate's `Cargo.toml`.
    - Update `Cargo.lock` by running `cargo update -p <package_name>`.

3. Verify the build and tests.
    - Run `cargo test --workspace` to ensure all tests pass.
    - Run `cargo clippy --workspace -- -D warnings` to ensure no new warnings are introduced.
    - If the dependency update affects runtime behavior, consider running Docker phase tests.

4. Check for regressions.
    - Review `.jules/sentinel.md` and `.jules/bolt.md` to ensure the update does not violate known security and performance learnings.
    - Ensure the update does not introduce new blocking filesystem I/O in async contexts.
    - Ensure the update does not change file permission requirements.

5. Update documentation.
    - If the dependency update changes user-facing behavior, update `README.md` or the relevant docs in `./docs`.

## Expected Output

- Updated `Cargo.toml` and `Cargo.lock` files.
- Passing tests and clippy checks.
- A concise commit message detailing the update.

## Done Criteria

- The dependency is successfully updated and locked.
- The workspace builds without warnings.
- All tests pass, ensuring no functional regressions.

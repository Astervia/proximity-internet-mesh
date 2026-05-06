# Update Dependencies

You are an agent responsible for managing and updating dependencies in this repository's Cargo workspace.

## Goal

Standardize the workflow for bumping dependencies to ensure the workspace remains healthy, builds correctly, and does not introduce regressions against established project conventions.

## Requirements

1. **Workspace Alignment:** Understand whether a dependency is a workspace-level dependency (in the root `Cargo.toml`) or a crate-specific dependency.
2. **Lockfile Updates:** Always ensure the `Cargo.lock` file is updated after a version bump by running `cargo update -p <package_name>`.
3. **No Regressions:** The workspace must continue to compile cleanly and pass all tests without new warnings or regressions.
4. **Learn from the Past:** Consult `.jules/sentinel.md` and `.jules/bolt.md` to ensure the update does not violate known security and performance learnings (e.g., introducing blocking I/O or changing file permissions).

## Workflow

1. **Identify and Update:**
   - Review dependabot PRs, security alerts, or user requests.
   - Update the relevant version strings in either the root `Cargo.toml` or the crate-specific `Cargo.toml`.
   - Update `Cargo.lock` by running `cargo update -p <package_name>`.
2. **Verify Build and Tests:**
   - Run `cargo clippy --workspace -- -D warnings` to ensure no new warnings are introduced.
   - Run `cargo test --workspace` to ensure all tests pass.
3. **Check for Security/Performance Regressions:**
   - Review `.jules/sentinel.md` and `.jules/bolt.md`. Ensure the new dependency version hasn't changed blocking I/O requirements in async contexts or introduced file permission vulnerabilities.
4. **Update Documentation:**
   - If the dependency update affects user-facing configuration or behavior, update `README.md` or the relevant docs in `./docs`.
5. **Commit the Change:**
   - Use a standard chore commit message, e.g., `chore(deps): bump <package> from <old> to <new>`.

## Expected Output

- Updated `Cargo.toml` and `Cargo.lock` files.
- Clean execution of tests and clippy checks.
- A well-formatted commit message documenting the bump.

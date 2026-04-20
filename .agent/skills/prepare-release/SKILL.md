# Skill: Prepare Release

Use this skill to automate the release preparation process, ensuring that all crates in the workspace are bumped consistently and properly locked.

## Purpose

Automate and standardize version bumping and `Cargo.lock` updates before creating a release tag. This ensures that only changed crates (and the master crate) receive version bumps, and helps prevent human error in semver tracking.

## Workflow

1. **Verify the environment**:
   - Ensure the repository is clean.
   - Run `cargo test --workspace` to ensure all tests pass.

2. **Run the prepare script**:
   - Execute the standard script: `./scripts/prepare-release.sh`.
   - The script will automatically compute whether `patch`, `minor`, or `major` bumps are required based on changes since the last release tag (`vX.Y.Z`). By default, it uses `patch`.
   - Optionally override bump levels if the release dictates it (e.g., `./scripts/prepare-release.sh --changed-bump minor`).

3. **Update the lockfile**:
   - Since `Cargo.toml` files are modified by the script, run: `cargo check` or `cargo build` to ensure the `Cargo.lock` file is updated to match the new local workspace versions.

4. **Verify the final changes**:
   - Run `git diff` to confirm that only the expected `Cargo.toml` and `Cargo.lock` files are modified.

## Expected Output

- Updated `Cargo.toml` files for any changed crates and the master crate.
- An updated `Cargo.lock` file containing the new local crate versions.

## Done Criteria

- The `Cargo.toml` versions are successfully bumped.
- The `Cargo.lock` file accurately reflects the bumped workspace dependencies.
- No other unintended files are modified.

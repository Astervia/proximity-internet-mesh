# Skill: Prepare Release

Use this skill when preparing to cut a new release for the repository, which involves version bumps across the workspace and updating dependencies locks.

## Purpose

Standardize the pre-release workflow using the provided `scripts/prepare-release.sh` tool. This ensures all modified crates are bumped systematically and the workspace remains consistent before tagging a release.

## Workflow

1. Run the prepare-release script.
    - Execute `./scripts/prepare-release.sh` from the repository root.
    - This script checks Git tags, determines which crates have changed since the last release, and bumps their versions in `Cargo.toml`.
    - It can be customized with options like `--changed-bump minor` or `--dry-run` to preview changes.

2. Review the output and modifications.
    - Inspect the output to verify which crates were bumped.
    - Check the `git diff` for changes in `Cargo.toml` files.

3. Update Cargo.lock.
    - Run `cargo update --workspace` or `cargo build` to ensure the `Cargo.lock` reflects the new versions.

4. Verify changes.
    - Run `cargo test --workspace` to ensure the workspace still builds and tests pass.
    - Stage the changes (`Cargo.toml` files and `Cargo.lock`).

5. Finalize the release manually.
    - The script does *not* create commits or tags. It prepares the files.
    - You or the human maintainer must create a commit (e.g., `chore: prepare release vX.Y.Z`) and explicitly tag the release.

## Expected Output

- Updated `Cargo.toml` files for changed crates.
- An updated `Cargo.lock`.
- Output describing the specific bumps made.

## Done Criteria

- The script completed successfully.
- Version bumps align with the expected changes.
- The `Cargo.lock` is up-to-date and all tests pass.

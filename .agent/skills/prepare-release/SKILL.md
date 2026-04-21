# Skill: Prepare Release

Use this skill when preparing a new workspace release. This ensures workspace crates are version bumped consistently and `Cargo.lock` is kept up to date.

## Purpose

Standardize the workflow for bumping crate versions prior to a release. We use the `scripts/prepare-release.sh` script to automate finding the latest tag, identifying which crates have changed, bumping those crates, and updating the master crate version.

## Workflow

1. Run the release preparation script.
   Execute `scripts/prepare-release.sh`. By default, this script:
   - Fetches tags from `origin`.
   - Finds the latest `vX.Y.Z` tag.
   - Identifies crates under `crates/` that have changed since the latest tag.
   - Bumps changed crates by a patch version.
   - Force-bumps the master crate (`pim-cli`) to the next version.

   You can override behavior with options:
   - `--changed-bump <kind>`: set kind to `patch`, `minor`, or `major`.
   - `--master-bump <kind>`: set kind to `patch`, `minor`, or `major`.
   - `--dry-run`: see what will happen without making changes.

2. Review Manifest Changes.
   - Verify `Cargo.toml` files have been properly updated.
   - Run `cargo check --workspace` to ensure things are syntactically correct after the version update.

3. Update Cargo.lock.
   - Run `cargo update -p <crate_name>` or simply run `cargo build` to ensure `Cargo.lock` reflects the bumped crate versions within the workspace.

4. Verify Tests.
   - Run `cargo test --workspace` to ensure no issues were caused by the bump (e.g. version parsing tests).

5. Next steps (Manual).
   - Once automated updates are done, you will commit the changes and inform the human to create the release tag manually.

## Expected Output

- Updated `version` fields in workspace `Cargo.toml` files.
- An updated `Cargo.lock` file.
- A descriptive commit of the version bumps.

## Done Criteria

- Crates are successfully bumped.
- Workspace builds successfully and tests pass.

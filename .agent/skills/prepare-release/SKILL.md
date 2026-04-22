# Skill: Prepare Release

Use this skill to guide the workflow for releasing a new version of the workspace. This is primarily driven by the `scripts/prepare-release.sh` utility.

## Purpose

To manage workspace version bumps, update lockfiles, and ensure consistent releases prior to final tagging and pushing. This guarantees standard conventions are adhered to and nothing is missed.

## Prerequisites

- The workspace must be in a clean state and currently building correctly (`cargo test --workspace` and `cargo clippy --workspace -- -D warnings` must pass).
- You should understand the types of changes being released to correctly select `patch`, `minor`, or `major` version bumps.

## Workflow

1. Execute the release preparation script.
   Run `scripts/prepare-release.sh`. By default, this script will:
     - Fetch latest tags from `origin`.
     - Detect changes in workspace crates compared to the last release.
     - Bump changed crates by a `patch` version.
     - Bump the master crate (default: `pim-cli`) to its next `patch` version, driving the release.

   *Options:*
     - `--dry-run`: View proposed bumps without editing files.
     - `--changed-bump <kind>`: specify `minor` or `major` for changed crates.
     - `--master-bump <kind>`: specify `minor` or `major` for the overall release.
     - `--master-crate <name>`: override the root crate to use as the master version.

2. Review the resulting manifest (`Cargo.toml`) changes.
   Use `git diff` to confirm version updates are correctly applied to `Cargo.toml` files for changed crates and the master crate.

3. Update the Cargo lockfile.
   Run `cargo update --workspace` or `cargo build` to generate an updated `Cargo.lock` that reflects the new local crate versions.

4. Verify stability.
   Run validations: `scripts/pre-pr-check.sh` and/or `cargo test --workspace` to ensure no regressions have been introduced by version bumping.

5. Prepare the release commit.
   Stage the `Cargo.toml` files and `Cargo.lock`. Create a release commit manually, typically named `chore(release): vX.Y.Z` where `X.Y.Z` is the new master crate version.

## Expected Output

- Updated `Cargo.toml` versions for modified workspace crates and the master crate.
- An updated `Cargo.lock`.
- Verification that all validations and tests are passing.
- Clear, step-by-step readiness for the user to perform the final manual steps: committing, tagging, and pushing.

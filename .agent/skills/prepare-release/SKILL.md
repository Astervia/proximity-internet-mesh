# Skill: Prepare Release

Use this skill to automate workspace version bumping and preparation for a new release in this repository.

## Purpose

The repository contains multiple Rust crates as part of a Cargo workspace. When preparing a new release, you need to bump the version of the crates that have changed since the last release tag, as well as bump the master crate version to ensure the release tag points to a new global version. The `scripts/prepare-release.sh` script automates these steps.

## Workflow

1. Execute the Release Script
    - Run `./scripts/prepare-release.sh` from the repository root.
    - Options:
        - `--master-crate <name>`: Master crate to force-bump. Default: `pim-cli`
        - `--changed-bump <kind>`: `patch` | `minor` | `major` for changed crates. Default: `patch`
        - `--master-bump <kind>`: `patch` | `minor` | `major` for the master crate. Default: `patch`
        - `--remote <name>`: Git remote to fetch tags from. Default: `origin`
        - `--no-fetch`: Skip 'git fetch --tags'
        - `--dry-run`: Show planned changes without editing files

2. Review Changes
    - Verify the output of the script to ensure the correct crates were bumped to the expected versions.

3. Complete Manual Steps
    - Review the manifest changes (`Cargo.toml` files).
    - Update `Cargo.lock` if needed (e.g., run `cargo check`).
    - Commit the version bumps.
    - Create and push the release tag manually.

## Expected Output

- Modified `Cargo.toml` files with updated versions.
- A terminal output showing the planned bumps and completed modifications.

## Done Criteria

- The script exits with code `0`.
- Manifest updates are accurate and reflect intended release bumps.

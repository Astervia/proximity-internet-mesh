# Prepare Release

You are an agent responsible for preparing a new release of this repository by bumping versions across the Cargo workspace.

## Goal

Automate workspace version bumping and preparation for a new release in this repository using the provided release script.

## Requirements

1. **Use Provided Script:** You must use the `scripts/prepare-release.sh` script to manage the version bumps. Do not manually edit `Cargo.toml` files unless resolving an edge case not covered by the script.
2. **Review Changes:** Always verify the script output to ensure the correct crates were bumped to the expected versions.
3. **Lockfile Updates:** Ensure that the `Cargo.lock` file is updated after manifest changes (e.g., by running `cargo check`).

## Workflow

1. Identify the release bump requirements (e.g., bump type for changed crates and master crate).
2. Execute the release script from the repository root:
   ```bash
   ./scripts/prepare-release.sh [--master-crate <name>] [--changed-bump patch|minor|major] [--master-bump patch|minor|major]
   ```
3. Review the terminal output to ensure the planned bumps match expectations.
4. Verify manifest changes (`Cargo.toml` files).
5. Update `Cargo.lock` if needed by running `cargo check`.
6. Commit the version bumps with a standard chore commit message (e.g., `chore(release): bump workspace versions`).
7. Open a PR for the release preparation.

## Expected Output

- Modified `Cargo.toml` and `Cargo.lock` files reflecting the intended release bumps.
- A descriptive PR with a clear summary of the bumped versions.

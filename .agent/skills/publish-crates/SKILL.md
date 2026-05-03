# Skill: Publish Crates

Use this skill to automate the publishing of workspace crates to crates.io.

## Purpose

After a release has been prepared, versions bumped, and changes committed/tagged, the crates in the workspace need to be published to crates.io in their topological dependency order. The `scripts/publish-crates.sh` script automates this process while handling pre-publish checks and rate limiting.

## Workflow

1. Ensure Prerequisites
    - Ensure `prepare-release` steps are completed.
    - Ensure the working tree is clean (no uncommitted changes).
    - Ensure you are authenticated with crates.io (if running locally or required by the environment).

2. Execute the Publish Script
    - Run `./scripts/publish-crates.sh` from the repository root.
    - Environment Variables (Optional):
        - `DRY_RUN=1`: Performs a dry-run to verify the process without making network writes.
        - `START_FROM=<crate_name>`: Resumes publishing from a specific crate (e.g., `START_FROM=pim-bluetooth`) if the process was interrupted (e.g., by a rate limit).
        - `PUBLISH_DELAY=<seconds>`: Configures the wait time between crate publishes. Defaults to 600 seconds (10 minutes) to respect crates.io rate limits for new crates, but can be lowered for subsequent version bumps.

3. Verify Publish Status
    - Monitor the script's output to ensure all crates are published successfully without errors.
    - The script automatically runs `cargo fmt`, `cargo clippy`, and `cargo test` before publishing.

## Expected Output

- Console output detailing the pre-publish checks, the order of published crates, and a final success message.

## Done Criteria

- The script exits with code `0`.
- All crates in the workspace are successfully published to crates.io.

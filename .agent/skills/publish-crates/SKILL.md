# Skill: Publish Crates

Use this skill to publish the workspace crates to crates.io after a release has been prepared.

## Purpose

The repository contains multiple Rust crates as part of a Cargo workspace that must be published in a specific topological dependency order. The `scripts/publish-crates.sh` script automates this process while properly respecting crates.io rate limits and ensuring pre-publish checks are passed.

## Prerequisites

1. You should have already prepared a release (bumped versions, updated lockfiles) and pushed the release tag.
2. Ensure you have no uncommitted changes in your working directory.

## Workflow

1. Execute the Publish Script
    - Run `./scripts/publish-crates.sh` from the repository root.
    - Options:
        - `DRY_RUN=1`: Perform a dry run without actually publishing to crates.io. Useful for validating pre-publish checks and publish order.
        - `START_FROM=<crate_name>`: Resume the publish process from a specific crate (e.g., `START_FROM=pim-bluetooth`). This is useful if the publish process was interrupted due to a rate-limit error or network issue.
        - `PUBLISH_DELAY=<seconds>`: Set a custom delay between publishing each crate. By default, this is 600 seconds (10 minutes) to accommodate crates.io's rate limits for *new* crate registrations. For subsequent version bumps of already registered crates, you can override this with a shorter delay (e.g., `PUBLISH_DELAY=30`).

2. Review Execution
    - The script will first run formatting, clippy, and tests.
    - It will then iterate through the predefined topological list of crates and publish each one sequentially.
    - Wait for the script to finish and emit "All crates published successfully."

## Expected Output

- Clean execution of pre-publish checks (fmt, clippy, test).
- Sequential logging of each crate being published and the delay timer.

## Done Criteria

- The script exits with code `0`.
- All crates are available in their updated versions on crates.io.

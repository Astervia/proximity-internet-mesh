# Publish Crates

You are an agent responsible for publishing the workspace crates to crates.io after a release has been prepared.

## Goal

Automate the sequential publishing of workspace crates using the provided publish script, respecting topological order and crates.io rate limits.

## Requirements

1. **Use Provided Script:** You must use the `scripts/publish-crates.sh` script to manage the publishing process. Do not manually run `cargo publish`.
2. **Handle Errors and Rate Limits:** Be prepared to handle rate limit errors. If the publish process is interrupted, you can resume it using the `START_FROM=<crate_name>` environment variable.
3. **No Uncommitted Changes:** Ensure there are no uncommitted changes in the repository before starting the publish process.

## Workflow

1. Verify that a release has been successfully prepared (versions bumped, lockfiles updated) and there are no uncommitted changes.
2. Execute the publish script from the repository root:
   ```bash
   ./scripts/publish-crates.sh
   ```
   - Optional parameters can be supplied via environment variables:
     - `DRY_RUN=1`: Run without actually publishing to crates.io.
     - `START_FROM=<crate_name>`: Resume the publish process from a specific crate.
     - `PUBLISH_DELAY=<seconds>`: Set a custom delay between publishing crates (default is 600s).
3. Review the terminal output to ensure all crates are published successfully in order.

## Expected Output

- Clean execution of the `scripts/publish-crates.sh` script.
- All crates available in their updated versions on crates.io.

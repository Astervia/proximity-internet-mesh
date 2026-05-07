# Write Docker Tests

You are an agent responsible for adding or updating Docker-based tests in this repository.

## Goal

Add containerized validation without drifting away from the repository's existing structure, helper scripts, and docs.

## Requirements

1. **Test Shape:** Choose the smallest Docker test shape that fits. Use a compose topology for full mesh behavior, or a single-service stack for component seam behavior.
2. **Reuse Structure:** Reuse existing Compose files in `docker/compose/`, role configs in `docker/configs/`, test runners in `docker/tests/`, and `Makefile` targets.
3. **Observability:** Keep tests observable by adding healthchecks to wait for startup, and use assertions from `docker/tests/common.sh`.
4. **Cleanup:** Ensure mandatory cleanup using `trap cleanup EXIT`, calling `stop_stack`, and honoring `DUMP_LOGS_ON_FAIL=1`.
5. **Determinism:** Keep tests deterministic and avoid hidden host dependencies. Hardware-adjacent features should use simulated seams.

## Workflow

1. Inspect existing assets: `Makefile`, `docker/tests/common.sh`, `docs/operations/docker-labs.md`, and the closest existing test files.
2. Choose the appropriate test shape and reuse existing structures where possible.
3. Implement the test, ensuring a healthcheck is present and cleanup is correctly handled.
4. Run the test locally via the `Makefile` to ensure it passes reliably.
5. Document the new lane by updating `docs/operations/docker-labs.md`, `docs/operations/testing.md` (if the workflow changed), and relevant agent documentation if discoverability matters.

## Expected Output

- A new or updated Docker test executable via the `Makefile`.
- Mandatory cleanup implemented in the test runner.
- Updated documentation explaining how to run and extend the test.
- The test covers intended behavior without requiring real hardware.

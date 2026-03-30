# Skill: Write Docker Tests

Use this skill when adding or updating Docker-based tests in this repository.

## Purpose

Add containerized validation without drifting away from the repository's
existing structure, helper scripts, and docs.

## Inspect First

- `Makefile`
- `docker/tests/common.sh`
- `docs/operations/docker-testing.md`
- The closest existing file under `docker/compose/`
- The closest existing script under `docker/tests/`

## Workflow

1. Choose the smallest Docker test shape that fits.
    - Full mesh behavior: add or reuse a compose topology.
    - Component seam behavior: prefer a smaller single-service stack.

2. Reuse the existing structure.
    - Compose files in `docker/compose/`
    - Role configs in `docker/configs/`
    - Test runners in `docker/tests/`
    - `Makefile` targets for build, run, logs, cleanup

3. Keep the test observable.
    - Add a healthcheck so scripts know when startup is complete.
    - Prefer assertions through `common.sh` helpers.
    - Add one new helper there only if multiple tests would benefit.

4. Make cleanup mandatory.
    - Use `trap cleanup EXIT`
    - Call `stop_stack`
    - Honor `DUMP_LOGS_ON_FAIL=1`

5. Document the new lane.
    - Update `docs/operations/docker-testing.md`
    - Update `docs/operations/testing.md` if the workflow changed
    - Update `agent/README.md` or `agent/skills/README.md` if discoverability matters

## Repository-Specific Rules

- Prefer `make test-...` targets over ad hoc commands.
- Keep Docker tests deterministic; avoid hidden host dependencies where possible.
- For hardware-adjacent features, add a seam that can be simulated in a container.
- Do not require real Wi-Fi or Bluetooth devices for default Docker coverage.

## Example Patterns

- Multi-node mesh test:
    - compose topology + static config files + shell assertions
- Component seam test:
    - one container + fake sysfs or controlled fixture + log/status assertions

## Done Criteria

- The new Docker test runs from `Makefile`
- Cleanup is automatic
- Docs explain how to run and extend it
- The test covers the intended behavior without requiring real hardware

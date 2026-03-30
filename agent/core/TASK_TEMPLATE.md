# Task Template

Use this template for scoped agent work in this repository.

## Goal

State the feature or defect in one sentence.

## Constraints

- Preserve the current transport/session pipeline unless the task requires a transport change.
- Avoid unrelated refactors.
- Keep config backward compatible where practical.

## Steps

1. Trace the current code path.
2. Identify the narrowest extension seam.
3. Implement config and service wiring.
4. Add or update tests.
5. Update architecture docs and operator docs as needed.

## Deliverables

- code changes
- tests or explicit testing gaps
- docs updates
- residual risks

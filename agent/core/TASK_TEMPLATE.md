# Task Template

Use this template for scoped agent work in this repository.

## Goal

State the task in one sentence.

## Constraints

- Preserve existing crate boundaries unless the task justifies a change.
- Avoid unrelated refactors.
- Keep config and operator behavior backward compatible where practical.

## Steps

1. Trace the current code path.
2. Identify the owning crate and the narrowest edit surface.
3. Implement the code change and any required wiring.
4. Add or update tests and verification steps.
5. Update docs when user-visible behavior, architecture, or workflows changed.

## Deliverables

- code changes
- tests or explicit testing gaps
- docs updates
- residual risks

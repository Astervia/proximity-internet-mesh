# Implement Change

Implement a scoped change in this repository.

Requirements:

- Trace the current behavior before editing.
- Pull broader repository context from `README.md` and `./docs` before inventing new assumptions.
- Review `.jules/sentinel.md` and `.jules/bolt.md` to ensure the change avoids regressions against known security and performance project learnings.
- Change the smallest reasonable surface that solves the task.
- Preserve crate boundaries unless the task clearly requires otherwise.
- Keep config and operator-facing behavior backward compatible where practical.
- Add or update tests that cover the changed behavior.
- Update docs when the change affects usage, architecture, or operations.

Repository-specific guidance:

- `pim-daemon` is the runtime integration point for most cross-crate behavior.
- `pim-core` owns shared config, common types, and error vocabulary.
- `.agent/core/DOCS_MAP.md` points to the general documentation in `./docs`.
- Docker phase tests matter for changes that affect multi-node or runtime behavior.

Expected output:

1. Short implementation plan.
2. Concrete code changes.
3. Verification results.
4. Risks, limitations, and follow-up work.

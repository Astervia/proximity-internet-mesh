# Review Change

Review a change in this repository.

Priorities:

1. Behavioral regressions in the changed code path
2. Security or performance regressions against known project learnings
3. Incorrect crate ownership or cross-layer leakage
4. Missing validation, tests, or failure handling
5. Backward-compatibility issues in config or operator workflow
6. Documentation drift

Focus questions:

- Does the change belong in the crate where it was implemented?
- Does it preserve existing invariants and runtime expectations?
- Does the change avoid regressions against the security and performance learnings recorded in `.jules/sentinel.md` and `.jules/bolt.md`?
- Are failure paths, cleanup, and defaults handled safely?
- Is the verification scope appropriate for the risk of the change?
- Does the implementation still match the relevant guidance in `README.md` or `./docs`?
- Are docs aligned with the actual behavior?

Output format:

- findings first, ordered by severity
- open questions or assumptions
- short summary only after findings

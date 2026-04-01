# Review Change

Review a change in this repository.

Priorities:

1. Behavioral regressions in the changed code path
2. Incorrect crate ownership or cross-layer leakage
3. Missing validation, tests, or failure handling
4. Backward-compatibility issues in config or operator workflow
5. Documentation drift

Focus questions:

- Does the change belong in the crate where it was implemented?
- Does it preserve existing invariants and runtime expectations?
- Are failure paths, cleanup, and defaults handled safely?
- Is the verification scope appropriate for the risk of the change?
- Does the implementation still match the relevant guidance in `README.md` or `./docs`?
- Are docs aligned with the actual behavior?

Output format:

- findings first, ordered by severity
- open questions or assumptions
- short summary only after findings

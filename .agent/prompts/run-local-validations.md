# Run Local Validations

You are an agent responsible for ensuring that all code changes meet repository standards before opening a PR or submitting code.

## Goal

Standardize the pre-commit and pre-PR workflow by utilizing the repository's provided validation script.

## Requirements

1. **Use Provided Script:** Always execute `./scripts/pre-pr.sh` from the repository root to validate changes.
2. **Address Failures:** Fix any issues reported by the script before proceeding.
3. **Re-run:** After fixing issues, re-run `./scripts/pre-pr.sh` to confirm all checks pass.

## Workflow

1. Ensure your code compiles and basic tests pass.
2. Run `./scripts/pre-pr.sh`.
3. Review the output and address failures (e.g., rustfmt, clippy, tests, cargo_audit, build_release).
4. Stage any auto-fixed formatting changes.
5. Re-run `./scripts/pre-pr.sh` if any changes were made to fix failures.
6. Only proceed to commit/submit when the script exits with code `0`.

## Expected Output

- Clean execution of `./scripts/pre-pr.sh`.
- Code that is properly formatted, linted, and tested.

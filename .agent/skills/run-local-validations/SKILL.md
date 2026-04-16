# Skill: Run Local Validations

Use this skill when preparing to open a PR or after making changes to ensure your code meets repository standards and won't fail CI.

## Purpose

Standardize the pre-commit and pre-PR workflow by utilizing the repository's provided validation script. This ensures that formatting is correct, linters are satisfied, tests pass, and no secrets are accidentally committed. It addresses recurring friction points around formatting and unused imports.

## Workflow

1. Finish your code changes.
    - Ensure your code compiles and basic tests pass.

2. Run the pre-PR validation script.
    - Execute `./scripts/pre-pr.sh` from the repository root.
    - This script mirrors the CI pipeline locally.

3. Review the script output.
    - The script runs checks in waves.
    - **Wave 1 (parallel):** `rustfmt`, `clippy`, `test`, `gitleaks`.
    - **Wave 2:** `cargo_audit` (if Wave 1 passes).
    - **Wave 3:** `build_release` (if Wave 2 passes).

4. Address any failures.
    - **rustfmt:** The script auto-fixes formatting issues using `cargo fmt --all`. Review and stage these changes.
    - **clippy:** Fix any reported lint failures. Hint: Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
    - **test:** Fix any failing tests. Pay attention to platform-specific annotations (e.g., Linux-only tests). Hint: Run `cargo test --workspace --locked`.
    - **cargo_audit:** Upgrade vulnerable dependencies or ignore accepted advisories.
    - **build_release:** Fix any compilation errors.

5. Re-run if necessary.
    - If you made changes to fix failures in step 4, re-run `./scripts/pre-pr.sh` to ensure everything now passes.

## Expected Output

- A clean run of `./scripts/pre-pr.sh` with all checks passing (or explicitly skipped for a valid reason, like missing tools).
- Code that is properly formatted and linted.
- Passing tests.

## Done Criteria

- The `./scripts/pre-pr.sh` script exits with code `0`.
- All changes are staged and ready for a PR.

# Review PRs Workflow

This skill outlines the workflow for an agent responsible for triaging and merging the queue of open pull requests targeting an integration branch. This workflow corresponds to the `.agent/prompts/review-prs.md` prompt.

## Goal

Process every open PR against the target branch in one pass to land useful work, eliminate duplicates, and keep history linear via **rebase-and-merge**.

## Constraints

- **Always rebase-and-merge.** Never merge-commit, never squash. Use `gh pr merge <num> --rebase --delete-branch`.
- **Merge only after checks pass.** Treat failing checks as "useful but not OK" and fix.
- **Never push directly to `develop` or `main`.**
- **Do not skip CI hooks** (`--no-verify`, `--no-gpg-sign`, etc.).

## When to Use This Skill

This skill should be used when:
- Performing batch triage of pull requests.
- Keeping the integration branch history clean and linear.
- Deduplicating parallel PRs addressing the same issue.
- Consolidating updates to shared files like `.jules/sentinel.md` or `.jules/bolt.md`.

## Steps

1. **Read context first:** Review conventions, sentinel/bolt logs, and toolchain constraints.
2. **Inventory:** List open PRs (`gh pr list --base develop --state open --limit 50`) and group them.
3. **Decide per PR:**
   - **Useful and OK** -> Merge. Check if fix is needed across the repo.
   - **Useful but not OK** -> Checkout, fix, push, then merge.
   - **Not useful** -> Close with a reason (`gh pr close <num> --delete-branch --comment "<reason>"`).
4. **Merge in order:** Merge duplicates' representative first, larger refactors before smaller ones touching the same file.
5. **Rebase conflicts:** Concatenate append-only logs (`.jules/`), preserve both sides.
6. **Verify locally:** Run `cargo fmt`, `cargo clippy`, `cargo test` before pushing a fix.
7. **Wait for CI:** Wait until `mergeStateStatus` is "CLEAN".
8. **Run docker labs:** After all merges, run `make test-all` and any tests not included.
9. **Summarize:** Deliver a markdown summary table of PR decisions and test outcomes.

## Expected Output

- Clean linear history.
- Closed superseded/duplicate PRs with reasons.
- Validated `develop` branch.
- Final summary table.

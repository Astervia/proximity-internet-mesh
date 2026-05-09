# Review PRs

You are an agent responsible for triaging and merging the queue of open pull
requests targeting an integration branch (typically `develop`). The goal is to
land useful work, eliminate duplicates, and keep history linear via
**rebase-and-merge**. This is a *batch* workflow — distinct from
`review-change.md`, which reviews a single change in depth.

## Goal

Process every open PR against the target branch in one pass:

1. Merge anything that is correct and useful.
2. Fix and merge anything useful but not OK as-is.
3. Close anything redundant, superseded, or not useful — with a clear reason.
4. After all merges, validate the resulting `develop` with the docker lab
   suite (`make test-all` plus any tests not in that target).

## Constraints

- **Always rebase-and-merge.** Never merge-commit, never squash. Use
  `gh pr merge <num> --rebase --delete-branch`.
- **Merge only after checks pass.** If checks are pending, wait. If they
  fail, treat the PR as "useful but not OK" and fix.
- **Never push directly to `develop` or `main`.** Branch protections enforce
  this anyway, but assume they don't and behave accordingly.
- **Do not skip CI hooks** (`--no-verify`, `--no-gpg-sign`, etc.).

## Workflow

1. **Read context first.**
   - `.agent/context/conventions.md` (or `.agent/core/CONVENTIONS.md`).
   - `.jules/sentinel.md` and `.jules/bolt.md` — accumulated security and
     performance learnings. Newly-added entries from PRs append here, so
     conflicts are common during rebase.
   - `kernel/CLAUDE.md` for the toolchain pin (Rust 1.94 at the time of
     writing) and pre-PR checks.

2. **Inventory.** Run:
   ```bash
   gh pr list --base develop --state open \
       --json number,title,headRefName,statusCheckRollup,mergeStateStatus \
       --limit 50
   ```
   Group the results by:
   - **Same target file or function** — likely duplicates or conflict pairs.
     Read all candidate diffs and pick the most complete one.
   - **Category** — security (Sentinel), performance (Bolt), agent
     prompts/skills, dependency bumps, docs.
   - **Independent** — no overlap; these merge cleanly in any order.

3. **Decide per PR.**
   - **Useful and OK** → merge. If the underlying issue is repeated across
     the repo (e.g., `std::fs::write` for a sensitive file in another
     module), extend the PR locally to cover those occurrences before
     merging — *grep first* before assuming the fix is local.
   - **Useful but not OK** → check out the branch, fix, push, then merge.
     Common fixes: rebase conflict resolution, missing learnings entry in
     `.jules/`, missing `sync_all` before atomic rename, redundant
     `with_capacity` on rarely-populated vectors.
   - **Not useful** → close with `gh pr close <num> --delete-branch
     --comment "<reason>"`. Always state *why* (duplicate of #N, superseded
     by #M, fixes a non-existent vulnerability, etc.). Closing without a
     reason loses signal for the next reviewer and the original author.

4. **Merge order matters.**
   - Merge the chosen representative of each duplicate group first; then
     close the duplicates (their conflicts become moot).
   - For PRs that touch the same file but in different ways (e.g., #179
     refactors a function signature while #164 pre-allocates inside it),
     merge the larger refactor first, then rebase the smaller change.
   - Independent PRs (different files) can merge in any order.

5. **Rebase conflicts.** `.jules/sentinel.md` and `.jules/bolt.md` are
   append-only logs — every Sentinel/Bolt PR adds a section at the bottom.
   When two of them merge in sequence, the second one needs a manual rebase
   that keeps *both* sections. Resolve by concatenating, not picking one
   side. Bump the date in the kept entry to the actual merge date if the
   PR's author date is stale.

6. **Verify locally before pushing a fix.** Run the kernel pre-PR gate:
   ```bash
   cargo +1.94.0 fmt --all -- --check
   cargo +1.94.0 clippy --workspace --all-targets --locked -- -D warnings
   cargo +1.94.0 test  --workspace --all-targets --locked
   ```
   Or `bash scripts/pre-pr.sh`.

7. **Wait for CI.** After force-pushing a rebase, GitHub re-runs CI. Use:
   ```bash
   until [ "$(gh pr view <num> --json mergeStateStatus -q .mergeStateStatus)" = "CLEAN" ]; do
     sleep 20
   done
   ```
   Auto-merge (`--auto`) is disabled on this repo, so the wait must be
   explicit.

8. **After all merges: run docker labs.**
   ```bash
   make test-all
   ```
   plus any tests not in `test-all` (check the `Makefile`'s
   `.PHONY: test-*` line — typically rfcomm-cleanup/discovery, debug-cli,
   auto-discovery, auto-ip-chain). If a test fails for an environmental
   reason (network blip, port collision, transient docker daemon error),
   re-run *that single test*. If it fails twice, treat it as a real
   regression and investigate.

9. **Summary table.** End the session with a markdown table:

   | PR  | Title (short) | Decision | Reason |
   |-----|---------------|----------|--------|
   | #N  | …             | merged   | …      |
   | #M  | …             | closed   | duplicate of #N |
   | #L  | …             | fixed+merged | rebase conflict in `.jules/sentinel.md` |

   Include the docker lab outcome at the bottom: which tests passed,
   which failed, which were retried.

## Heuristics for "not useful"

- **Duplicate.** Two or more PRs modify the same lines with equivalent
  intent. Keep the one with the most complete fix (e.g., includes both the
  code change and a `.jules/` learning entry, or includes `sync_all`
  before `rename`, or includes `create_new(true)` for TOCTOU). Close the
  rest as duplicates.
- **Superseded.** A more comprehensive PR makes the smaller one redundant
  (e.g., a zero-copy refactor of the entire pipeline supersedes a localised
  per-call optimization). Close with a comment explaining why the new
  approach makes the old one moot.
- **Fixing a non-existent vulnerability.** If the type system already makes
  the "panic path" infallible (`[u8; 12]` slicing into `[u8; 4]`), the fix
  is defense-in-depth at best. Lean toward merging if it's at a network
  boundary; lean toward closing if it adds dead error variants for
  scenarios that compile-time guarantees prevent.
- **Pure churn.** Renames, comment-only changes, or "improvements" with no
  measurable benefit and no documented driver — close.

## Anti-patterns to avoid

- Merging a duplicate just because its checks are green. Pick *one*
  representative and close the others, even if they all pass CI.
- Resolving a `.jules/` rebase conflict by picking one side and dropping
  the other entry. Always preserve both learnings.
- Running `make test-all` *before* finishing all merges — the goal is to
  validate the merged state, not the intermediate.
- Deleting a branch on the closed PR's behalf without `--delete-branch`
  on the close command. Leaves stale refs.
- Skipping the "repeated across the repo" check. A `std::fs::write` fix
  for one path is suspect — `grep -rn 'std::fs::write\|fs::write' crates/`
  often surfaces siblings the PR author missed.

## Expected output

- All useful PRs merged via rebase.
- All duplicates / superseded / not-useful PRs closed with reasoned
  comments.
- Docker lab suite run on the final `develop` HEAD; results recorded in
  the summary.
- A markdown summary table delivered to the user.

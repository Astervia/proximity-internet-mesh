# Review PRs Workflow

This skill outlines the workflow for an agent responsible for triaging and merging a batch of pull requests into an integration branch, typically `develop`. It corresponds to the `review-prs.md` prompt.

## Goal

Process open pull requests efficiently, resolving duplicates, preserving `.jules/sentinel.md` and `.jules/bolt.md` learnings, and rebasing-and-merging changes that are useful while closing unneeded ones. Validates that `develop` passes the test suite afterward.

## Requirements

1. **Rebase-and-merge Only:** Avoid squashing or merge commits. Always rebase.
2. **Review Strategy:** Group PRs effectively (e.g. by target file, security vs performance, etc.), pick the most complete version in case of duplicates, and close the rest with reasons.
3. **No Breakage:** Only merge PRs that pass CI. Wait for CI if forced to push a rebase.
4. **Learning preservation:** Keep accumulated security and performance learnings in `.jules/` logs intact when they conflict.

## When to Use This Skill

This skill should be invoked when taking on a session to clear a backlog of pull requests, specifically batch triage.

## Steps

1. **Context Check:** Review standard `.agent/` instructions as well as `.jules/` accumulated learnings.
2. **List Open PRs:** Retrieve PRs targeting `develop` (e.g. `gh pr list --base develop`). Group them logically.
3. **Evaluate and Decide:** For each PR group, determine whether to merge, fix then merge, or close as not useful/superseded.
4. **Merge Iteratively:** Rebase, resolve conflicts (especially keeping all changes from `.jules/sentinel.md` and `bolt.md`), push, wait for CI, and merge. Close duplicates.
5. **Run Lab Suite:** Run `make test-all` plus any untracked docker lab tests. If a test fails for environmental reasons, retry up to three times with logging.
6. **Generate Report:** Summarize decisions and test outcomes in a Markdown table.

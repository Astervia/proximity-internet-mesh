# Update Agent Workspace Workflow

This skill outlines the workflow for an agent responsible for iteratively improving the `.agent/` workspace in this repository.

## Goal

The objective is to analyze recent changes, recurring patterns, or missing automation in the repository, and implement *one small, easily reviewable* improvement to the `.agent/` folder (e.g., a new skill, a refined prompt, or updated core instructions).

## Requirements

1. **Scope:** Keep the change small and focused. The resulting PR must be easy for a human to review in less than 5 minutes.
2. **Relevance:** The improvement must be grounded in actual recent activity in the repository, or clear gaps in existing `.agent/` documentation/tools.
3. **Format:** Adhere to the established structure of `.agent/` (e.g., `core/`, `prompts/`, `skills/`, `tools/`).
4. **Documentation:** If adding a new prompt or skill, ensure its purpose and usage are clear.

## When to Use This Skill

This skill should be used when:
- Recurring patterns or friction points are observed in recent commits or pull requests that could benefit from an agentic workflow.
- A new automated prompt was added to `.agent/prompts/` but lacks a corresponding `.agent/skills/` entry.
- The repository structure or conventions change, requiring an update to `.agent/core/`.

## Steps

1. **Analyze:** Briefly review recent commits (e.g., using `git log -n 10 --oneline`) or pull requests to identify recurring tasks or friction points.
2. **Review:** Review the current state of `.agent/` directories (e.g., `skills/`, `prompts/`, `core/`).
3. **Propose:** Identify a single, specific addition or modification (e.g., "Add a `skills/update-dependencies/SKILL.md`").
4. **Implement:** Create or update the necessary file in the `.agent/` directory using the tools at your disposal.
5. **Verify:** Confirm the creation or modification of the file using a read tool.
6. **Test:** Run relevant tests (e.g., `cargo test --workspace`) to ensure no regressions were introduced.
7. **Document:** Ensure the added prompt or skill's purpose and usage are clearly documented.
8. **Pre-commit:** Complete pre-commit steps and validation.
9. **Submit:** Create a descriptive commit message and PR for the human reviewer.

## Examples of Valid Improvements

- Creating a new `skills/.../SKILL.md` that corresponds to an existing prompt.
- Adding a new `prompts/...md` for a repetitive development task.
- Modifying `core/CONVENTIONS.md` to document a newly discovered architectural constraint or best practice.

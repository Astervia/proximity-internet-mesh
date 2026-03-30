# Agent Workspace

This directory holds repo-local assets for agentic coding workflows.

## Layout

- `core/` — shared repository context, conventions, and task templates
- `prompts/` — reusable prompts for common engineering tasks
- `skills/` — optional domain-specific workflows for repeatable work
- `tools/` — helper scripts and inspection utilities

Keep this tree repo-local so prompts and automation stay aligned with the codebase.

## Default Usage

Start with the generic assets in `core/` and `prompts/` for most work:

- `core/CONVENTIONS.md` — repository-wide implementation rules
- `core/ARCHITECTURE_MAP.md` — workspace and runtime orientation
- `core/DOCS_MAP.md` — where to find broader reference material in `./docs`
- `core/TASK_TEMPLATE.md` — default template for scoped coding tasks
- `prompts/implement-change.md` — prompt for feature or refactor work
- `prompts/review-change.md` — prompt for code review work

Use `./docs` as the main source of broader repository context. The files under
`agent/` should stay concise and point to the relevant material in `./docs`
rather than duplicating it.

## Domain-Specific Assets

Feature-specific guidance belongs in `skills/`, targeted prompts, or helper
tools rather than in the default core guidance.

Current examples:

- `skills/add-connection-mechanism/SKILL.md` — workflow for adding a new peer connectivity mechanism
- `prompts/implement-connectivity-mechanism.md` — targeted prompt for transport or discovery work
- `prompts/review-connectivity-change.md` — targeted review prompt for connectivity-sensitive changes
- `tools/inspect-connectivity-surface.sh` — quick trace for discovery, transport, and daemon integration points

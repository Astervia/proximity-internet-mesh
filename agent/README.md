# Agent Workspace

This directory holds repo-local assets for agentic coding workflows.

## Layout

- `prompts/` — reusable task, planning, review, and execution prompts
- `skills/` — focused workflow guides or domain-specific instructions
- `tools/` — helper scripts, wrappers, and tool adapters for agents
- `core/` — shared conventions, schemas, templates, and baseline context

Keep this tree repo-local so prompts and automation stay aligned with the codebase.

## Starter Set

This repository includes a starter set for extending peer connectivity, with an
initial focus on adding Bluetooth P2P alongside the existing TCP transport plus
UDP discovery and Wi-Fi Direct peer-finding model.

- `core/CONVENTIONS.md` — repo-specific implementation rules for agent work
- `core/ARCHITECTURE_MAP.md` — where discovery, transport, and connection setup live
- `core/TASK_TEMPLATE.md` — default execution template for scoped engineering tasks
- `prompts/implement-connectivity-feature.md` — implementation prompt for new connectivity work
- `prompts/review-connectivity-change.md` — review prompt for transport/discovery changes
- `skills/add-connection-mechanism/SKILL.md` — workflow for adding a new peer connection mechanism
- `tools/inspect-connectivity-surface.sh` — quick codebase trace for relevant integration points

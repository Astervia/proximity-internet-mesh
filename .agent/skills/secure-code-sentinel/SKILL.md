# Secure Code Sentinel Workflow

Use this skill when performing security-focused fixes or reviews (the "Sentinel" workflow) to ensure consistency in PR formatting and codebase learning documentation.

## Requirements

1. **Avoid Exposing Critical Vulnerabilities in PRs:** Never expose critical vulnerability details in public PR descriptions.
2. **Limit Scope:** Ensure fixes are kept under 50 lines unless explicitly instructed otherwise.
3. **Avoid Large Refactors:** Avoid making large, breaking security refactors in a single pass.
4. **Permissions:** When handling sensitive file permissions (e.g. PID files, config files, keys), strictly adhere to the secure file permission conventions using `OpenOptionsExt::mode(0o600)` or similar as required by the OS.

## PR Formatting

When creating a security-focused PR, adhere to the following template:

**Title Format:**
- For severe issues: `🛡️ Sentinel: [CRITICAL] Fix [vulnerability]` or `🛡️ Sentinel: [HIGH] Fix [vulnerability]`
- For enhancements or lower severity: `🛡️ Sentinel: [security improvement]`

**PR Description Sections:**
Must include the following exact sections:
- `Severity`
- `Vulnerability`
- `Impact`
- `Fix`
- `Verification`

## Logging to `.jules/sentinel.md`

Whenever you complete a Sentinel task that involves a critical security learning, you must document it in `.jules/sentinel.md`. Do not journal routine fixes.

**Format for logging:**

```markdown
## YYYY-MM-DD - [Title]
**Vulnerability:** [What you found]
**Learning:** [Why it existed]
**Prevention:** [How to avoid next time]
```

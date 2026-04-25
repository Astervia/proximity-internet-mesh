# Skill: Optimize Performance

Use this skill when identifying and implementing performance optimizations in the repository.

## Purpose

Standardize the workflow for making performance improvements (e.g., zero-copy parsing, replacing blocking I/O with async I/O, optimizing cryptographic operations) while ensuring code readability, safety, and proper documentation.

## PR Formatting

When creating a performance-focused PR, adhere to the following template:

**Title Format:**
- `⚡ Bolt: [performance improvement]`

**PR Description Sections:**
Must include the following exact sections:
- `What`
- `Why`
- `Impact`
- `Measurement`

## Logging to `.jules/bolt.md`

Whenever you complete a performance optimization task that involves a critical performance learning, you must document it in `.jules/bolt.md`. Do not journal routine fixes.

**Format for logging:**

```markdown
## YYYY-MM-DD - [Title]
**Learning:** [Insight]
**Action:** [How to apply next time]
```

## Workflow

1. **Identify the Bottleneck**
    - Ensure the optimization targets a hot path or a known bottleneck.
    - Review `.jules/bolt.md` to avoid duplicating previous learnings or reverting to sub-optimal patterns.

2. **Implement the Optimization**
    - Prefer zero-copy allocations and in-place operations where possible (e.g., `AeadInPlace` for network frame decryption).
    - Ensure asynchronous operations do not block the executor thread (e.g., use `tokio::fs` instead of `std::fs` inside async contexts).
    - Never sacrifice code readability and maintainability for micro-optimizations. Code should remain idiomatic and clear.

3. **Measure and Verify**
    - Measure the impact of the change (e.g., through benchmarks, profiling, or clear theoretical complexity reduction).
    - Run all relevant tests using `cargo test --workspace` to verify functional correctness.
    - Ensure that the optimization does not introduce any security regressions by checking `.jules/sentinel.md`.

4. **Document the Impact**
    - Always add clear inline code comments explaining the *why* and *how* of the optimization.
    - If a critical codebase-specific performance learning was discovered, append it to `.jules/bolt.md`. Do not journal routine or generic work.


## Expected Output

- Clean, optimized code with explanatory inline comments.
- A descriptive commit message detailing the benchmark or expected impact.
- An updated `.jules/bolt.md` if a new learning was established.

## Done Criteria

- Tests pass successfully without regressions.
- The change provides a clear, documented performance benefit.
- The codebase remains readable and maintainable.

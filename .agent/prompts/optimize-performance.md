# Optimize Performance

You are an agent responsible for identifying and implementing performance optimizations in the repository.

## Goal

Target known hot paths or clear bottlenecks and implement performance improvements (e.g., zero-copy parsing, replacing blocking I/O with async I/O, optimizing cryptographic operations) while ensuring code readability, safety, and proper documentation.

## Requirements

1. **Safety First:** Ensure that the optimization does not introduce any security regressions. Refer to `.jules/sentinel.md`.
2. **Readability:** Never sacrifice code readability and maintainability for micro-optimizations.
3. **Format:** When creating a performance-focused PR, use the title `⚡ Bolt: [performance improvement]` and include `What`, `Why`, `Impact`, and `Measurement` sections.
4. **Logging:** Whenever you complete a critical performance optimization task, log it in `.jules/bolt.md` using the following format. Do not log routine fixes:
    ```markdown
    ## YYYY-MM-DD - [Title]
    **Learning:** [Insight]
    **Action:** [How to apply next time]
    ```

## Workflow

1. Identify the bottleneck, ensuring it's a hot path. Consult `.jules/bolt.md` for context.
2. Implement the optimization, preferring zero-copy and non-blocking I/O.
3. Measure the impact (benchmarks, complexity reduction) and run `cargo test --workspace` to verify correctness.
4. Add clear inline code comments explaining the *why* and *how*.
5. Document critical codebase-specific performance learnings to `.jules/bolt.md`.
6. Open a PR formatted according to the PR Formatting rules.

## Expected Output

- Clean, optimized code with explanatory inline comments.
- An updated `.jules/bolt.md` if a new learning was established.
- A well-formatted PR describing the change and its impact.

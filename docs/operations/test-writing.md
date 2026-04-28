# Test Writing Conventions

How to add and organize unit tests in this workspace. See [testing.md](testing.md) for how to _run_ the test suite and the broader test strategy across unit, Docker integration, and CI layers.

## Goal

Production source files contain production code only. Unit tests live next to the module they exercise, in a dedicated `tests/` folder, split by topic so individual files stay readable.

## Layout

For every module file `foo.rs` that has tests, the layout is:

```
foo.rs                          ← production only; ends with `#[cfg(test)] mod tests;`
foo/
├── tests.rs                    ← module file: declares submodules and shared helpers
└── tests/
    ├── happy_path.rs           ← one or more themed submodules
    └── error_paths.rs
```

For crate roots (`lib.rs` / `main.rs`), the sibling `tests.rs` lives directly next to it:

```
src/
├── lib.rs                      ← `#[cfg(test)] mod tests;`
├── tests.rs                    ← module file
└── tests/
    └── basics.rs
```

Submodules of `tests/` are _always_ in a folder, even if there's only one file. This way, splitting later is a no-friction rename.

## Wiring

The production file ends with:

```rust
#[cfg(test)]
mod tests;
```

The `tests.rs` module file declares each submodule:

```rust
mod happy_path;
mod error_paths;

// Shared helpers (optional) — visible to children by Rust's privacy rules:
fn make_peer() -> Peer { ... }
```

Each submodule under `tests/` reaches back into the production module via two-level `super`:

```rust
// foo/tests/happy_path.rs
use super::super::*;     // production items defined in foo.rs
use super::make_peer;    // helpers defined in foo/tests.rs
```

`use super::*;` from the parent `tests.rs` is only needed if `tests.rs` _itself_ references items from the production module (e.g. helpers that take production types as arguments). If `tests.rs` only declares submodules, omit the import — the compiler will warn it's unused.

## Submodule names

Group tests by **what they're testing**, not by individual function. Aim for files in the ~150–300 line range. Useful themes seen in this repo:

| Theme                                                                                                                                    | Example crate / file            |
| ---------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------- |
| `convergence`, `split_horizon`, `invalidation`                                                                                           | `pim-routing/src/table/tests/`  |
| `nat`, `conntrack`, `port_pool`, `firewall`                                                                                              | `pim-gateway/src/engine/tests/` |
| `send_recv`, `error_paths`, `lifecycle`                                                                                                  | `pim-transport/src/tcp/tests/`  |
| `parsers`, `interface`, `discovery`, `nap_server`, `errors`                                                                              | `pim-bluetooth/src/tests/`      |
| `backoff`, `observability`, `reconnect`, `auth`, `flow_control`, `gateway`, `peer_lifecycle`, `capabilities`, `bluetooth`, `wifi_direct` | `pim-daemon/src/app/tests/`     |
| `general`, `discovery`, `transport`, `relay`, `security`, `wifi_direct`, `bluetooth`, `peers`                                            | `pim-core/src/config/tests/`    |

If you can't think of a theme yet, start with one file (`basics.rs`) and split later as tests accumulate.

## Sharing helpers

Three layers of helper visibility, in order of preference:

1. **Local to one submodule** — define inline in the submodule itself.
2. **Shared across multiple submodules** — define in the parent `tests.rs`. Children automatically see private items of their parent module per Rust's privacy rules, so no `pub` visibility modifier is required.
3. **Shared with a `#[cfg(test)]`-only production helper** — use a sibling `test_util.rs` declared from the production module:

    ```rust
    // engine.rs
    #[cfg(test)]
    pub mod test_util;
    ```

    Example: `pim-gateway/src/engine/test_util.rs` exposes packet builders to both `engine/tests/` and (re-exported) the daemon crate's tests.

## Choosing what to put where

- **Unit tests** that need access to private items → child `tests` module of that file (`use super::super::*;`). This is the default and what the layout above describes.
- **Integration tests** that exercise only the public API → crate-level `tests/` directory (`crates/<name>/tests/<scenario>.rs`). None of the crates here use this yet — add one if you're testing across multiple modules through the public surface.
- **Cross-crate end-to-end tests** → Docker labs under `docker/tests/`. See [docker-labs.md](docker-labs.md).

## The `pim-daemon` `#[path]` exception

Most modules in `pim-daemon/src/app.rs` are loaded via `#[path = "X.rs"] mod X;`, which keeps daemon-internal source files as siblings under `src/` rather than nested under `src/app/`. This puts those files in _module-file mode_, where submodule lookups happen in the same directory rather than under a subdirectory.

Concretely: a plain `mod tests;` inside `send_buffer.rs` resolves to `src/tests.rs` (wrong) instead of `src/send_buffer/tests.rs`. Fix this by giving both the test module and the inner submodule explicit `#[path]` attributes:

```rust
// pim-daemon/src/send_buffer.rs
#[cfg(test)]
#[path = "send_buffer/tests.rs"]
mod tests;
```

```rust
// pim-daemon/src/send_buffer/tests.rs
#[path = "tests/basics.rs"]
mod basics;
```

This only applies to daemon files loaded from `app.rs` via `#[path]`. Files loaded normally (`mod app;` from `main.rs`, or anything in another crate) follow the standard layout without `#[path]` attributes.

## Adding tests to a new module

1. Create the test folder: `mkdir -p path/to/module/tests`.
2. Add `#[cfg(test)] mod tests;` to the bottom of the production file.
3. Create `path/to/module/tests.rs` with `mod <theme>;` declarations.
4. Create `path/to/module/tests/<theme>.rs` starting with `use super::super::*;`.
5. Run `cargo test -p <crate>` to verify.

## What not to do

- Do not put `#[cfg(test)] mod tests { ... }` blocks inline in production files. Move them out.
- Do not put a flat `tests.rs` next to a production file without a `tests/` folder. Even single-submodule modules use the folder so growth is easy.
- Do not use `tests/mod.rs`. The Rust 2018+ idiom this codebase follows is `tests.rs` + `tests/` directory.
- Do not leak test-only helpers into the production module's public API. Use `#[cfg(test)]` gating on shared `test_util.rs` modules.
- Do not split a test that genuinely tests one scenario into multiple files for size's sake — group by behavior, not lines.

## Verification

After editing tests, the workspace should still pass cleanly:

```bash
cargo test --workspace
```

Build with no warnings:

```bash
cargo build --workspace --tests
```

There should be no inline test modules anywhere in `crates/`:

```bash
grep -rn '^mod tests {' crates/   # expected: no matches
```

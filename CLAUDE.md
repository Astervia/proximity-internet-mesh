# Proximity Internet Mesh — Notas para Claude

## Pré-push obrigatório (antes de abrir/atualizar qualquer PR)

O CI (`.github/workflows/`) roda **Rust 1.94.0** (não a stable local). Sempre
reproduza os checks com a mesma toolchain antes de `git push`, ou um job vai
quebrar — em especial `Code Quality – Format, Clippy & Tests`.

```bash
cargo +1.94.0 fmt --all -- --check
cargo +1.94.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.94.0 test  --workspace --all-targets --locked
```

Se faltar a toolchain: `rustup toolchain install 1.94.0 --component clippy --component rustfmt`.

### Por que 1.94 e não a stable

- O CI fixa `dtolnay/rust-toolchain@1.94.0`. Rodar `cargo clippy` com a stable
  local (ex.: 1.95) pode disparar lints novos que o CI ignora **e** mascarar
  lints antigos (ex.: `doc_lazy_continuation`) que o CI ainda fiscaliza.
- Vários crates (`pim-gateway`, `pim-wifidirect`, `pim-tun`) têm código
  `#[cfg(target_os = "linux")]`. Lints só aparecem na plataforma certa — o CI
  é Linux, então um clippy clean no macOS **não garante** CI verde. O que
  garante é a toolchain travada + `--locked`.

### Checks que o CI cobre (referência rápida)

- `Code Quality` → fmt + clippy + test (Linux, Rust 1.94, `--locked`).
- `CodeQL`, `gitleaks`, `cargo-audit`, `SBOM` → segurança/supply-chain.
- `Platform Validation` → matriz multi-OS (só roda em alguns triggers).

Se um check falhar, leia o log com `gh run view <run-id> --log-failed` antes
de chutar correção.

//! Binary entrypoint for the PIM daemon.
//!
//! Real logic lives in the `pim_daemon` library so the same code can be
//! linked into mobile embeddings (Phase B). Behaviour for the
//! `bin/pim-daemon` standalone binary is unchanged.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pim_daemon::run_binary().await
}

//! Binary entrypoint for the PIM daemon.

mod app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}

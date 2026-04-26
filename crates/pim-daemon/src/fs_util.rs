use std::io;

pub(crate) async fn atomic_write(path: &str, content: &[u8]) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let tmp = format!("{path}.tmp");

    // Unconditionally remove the temp file to ensure O_CREAT respects the mode.
    let _ = tokio::fs::remove_file(&tmp).await;

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).await?;
    file.write_all(content).await?;
    file.flush().await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

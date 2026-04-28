use std::io;
use std::path::{Path, PathBuf};

pub(crate) async fn atomic_write<P: AsRef<Path>>(path: P, content: &[u8]) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let path = path.as_ref();

    // Sibling `.tmp` file in the same directory so the final `rename`
    // is atomic on POSIX.
    let mut tmp_buf = PathBuf::from(path);
    let tmp_name = match tmp_buf.file_name() {
        Some(n) => format!("{}.tmp", n.to_string_lossy()),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no file name",
            ))
        }
    };
    tmp_buf.set_file_name(tmp_name);
    let tmp = tmp_buf;

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

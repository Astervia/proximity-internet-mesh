//! Common error type shared across crates.

use thiserror::Error;

#[derive(Error, Debug)]
/// Cross-crate error wrapper used by higher-level components.
pub enum PimError {
    /// Cryptographic operation failed.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// Protocol parsing or validation failed.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Transport-layer operation failed.
    #[error("transport error: {0}")]
    Transport(String),

    /// Routing-layer operation failed.
    #[error("routing error: {0}")]
    Routing(String),

    /// TUN interface operation failed.
    #[error("tun error: {0}")]
    Tun(String),

    /// Gateway or NAT operation failed.
    #[error("gateway error: {0}")]
    Gateway(String),

    /// Configuration parsing or validation failed.
    #[error("config error: {0}")]
    Config(String),

    /// Filesystem or other I/O failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = PimError::Crypto("bad key".into());
        assert_eq!(format!("{err}"), "crypto error: bad key");
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let pim_err: PimError = io_err.into();
        assert!(format!("{pim_err}").contains("gone"));
    }
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PimError {
    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("routing error: {0}")]
    Routing(String),

    #[error("tun error: {0}")]
    Tun(String),

    #[error("gateway error: {0}")]
    Gateway(String),

    #[error("config error: {0}")]
    Config(String),

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

use std::{error, io};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(io::Error),
    Std(Box<dyn error::Error + Send + Sync>),
    InvalidUri(InvalidUri),
    Resolve,
    Timeout(TimeoutError),
    TlsNotEnabled,
    #[cfg(feature = "http2")]
    H2(h2::Error),
    #[cfg(feature = "openssl")]
    Openssl(_openssl::OpensslError),
}

#[cfg(feature = "openssl")]
mod _openssl {
    use super::Error;

    use openssl_crate::{error, ssl};

    #[derive(Debug)]
    pub enum OpensslError {
        Single(error::Error),
        Stack(error::ErrorStack),
        Ssl(ssl::Error),
    }

    impl From<error::Error> for Error {
        fn from(e: error::Error) -> Self {
            Self::Openssl(OpensslError::Single(e))
        }
    }

    impl From<error::ErrorStack> for Error {
        fn from(e: error::ErrorStack) -> Self {
            Self::Openssl(OpensslError::Stack(e))
        }
    }

    impl From<ssl::Error> for Error {
        fn from(e: ssl::Error) -> Self {
            Self::Openssl(OpensslError::Ssl(e))
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<Box<dyn error::Error + Send + Sync>> for Error {
    fn from(e: Box<dyn error::Error + Send + Sync>) -> Self {
        Self::Std(e)
    }
}

#[cfg(feature = "http2")]
impl From<h2::Error> for Error {
    fn from(e: h2::Error) -> Self {
        Self::H2(e)
    }
}

#[derive(Debug)]
pub enum InvalidUri {
    ReasonUnknown,
    MissingHost,
    MissingScheme,
    MissingAuthority,
    MissingPathQuery,
    UnknownScheme,
}

impl From<http::uri::InvalidUri> for InvalidUri {
    fn from(_: http::uri::InvalidUri) -> Self {
        Self::ReasonUnknown
    }
}

impl From<http::uri::InvalidUri> for Error {
    fn from(e: http::uri::InvalidUri) -> Self {
        Self::InvalidUri(e.into())
    }
}

impl From<InvalidUri> for Error {
    fn from(e: InvalidUri) -> Self {
        Self::InvalidUri(e)
    }
}

#[derive(Debug)]
pub enum TimeoutError {
    Resolve,
    Connect,
    TlsHandshake,
    Request,
}

impl From<TimeoutError> for Error {
    fn from(e: TimeoutError) -> Self {
        Self::Timeout(e)
    }
}

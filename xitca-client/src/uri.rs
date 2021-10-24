use std::ops::Deref;

use http::uri;

use crate::error::InvalidUri;

/// A new type of http::uri::Uri.
/// The purpose of it is to treat the Uri differently with Unix Socket Domain connection.
#[derive(Debug, Clone)]
pub enum Uri {
    Tcp(uri::Uri),
    Tls(uri::Uri),
    #[cfg(unix)]
    Unix(uri::Uri),
}

impl Deref for Uri {
    type Target = uri::Uri;

    fn deref(&self) -> &Self::Target {
        match *self {
            Self::Tcp(ref uri) => uri,
            Self::Tls(ref uri) => uri,
            #[cfg(unix)]
            Self::Unix(ref uri) => uri,
        }
    }
}

impl Uri {
    pub(crate) fn try_parse(uri: uri::Uri) -> Result<Self, InvalidUri> {
        match (uri.scheme_str(), uri.host(), uri.authority()) {
            (_, _, None) => Err(InvalidUri::MissingAuthority),
            (_, None, _) => Err(InvalidUri::MissingHost),
            (None, _, _) => Err(InvalidUri::MissingScheme),
            (Some("http" | "ws"), _, _) => Ok(Uri::Tcp(uri)),
            (Some("https" | "wss"), _, _) => Ok(Uri::Tls(uri)),
            #[cfg(unix)]
            (Some("unix"), _, _) => Ok(Uri::Unix(uri)),
            (Some(_), _, _) => Err(InvalidUri::UnknownScheme),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn uri_parse() {
        let err = Uri::try_parse(uri::Uri::from_static("http2://example.com"))
            .err()
            .unwrap();
        assert!(matches!(err, InvalidUri::UnknownScheme));

        let err = Uri::try_parse(uri::Uri::from_static("/hello/world")).err().unwrap();
        assert!(matches!(err, InvalidUri::MissingAuthority));

        let err = Uri::try_parse(uri::Uri::from_static("example.com")).err().unwrap();
        assert!(matches!(err, InvalidUri::MissingScheme));

        let _ = Uri::try_parse(uri::Uri::from_static("http://example.com")).unwrap();

        let uri = Uri::try_parse(uri::Uri::from_static("unix://tmp/foo.socket")).unwrap();
        assert_eq!(uri.scheme_str().unwrap(), "unix");
        assert_eq!(uri.host().unwrap(), "tmp");
        assert_eq!(uri.path(), "/foo.socket");
    }
}

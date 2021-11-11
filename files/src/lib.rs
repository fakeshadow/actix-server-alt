//! Static file serving Service.
//!
//! Source code is copy/paste from actix-files and modification are made to make it
//! more performant and simpler.

#![feature(generic_associated_types, type_alias_impl_trait)]

mod chunked;
mod directory;
mod files;
mod named;
mod path_buf;
mod utf8;

pub mod error;

pub use files::Files;
pub use named::NamedFile;

#[cfg(test)]
mod test {
    use xitca_web::{dev::ServiceFactory, service::HttpServiceAdaptor, App};

    use crate::files::Files;

    #[tokio::test]
    async fn app() {
        let app = App::new().service(HttpServiceAdaptor::new(Files::new("/", "./")));

        let _app = app.new_service(()).await.unwrap();
    }
}

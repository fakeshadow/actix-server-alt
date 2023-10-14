//! async buffer io trait for linux io-uring feature with tokio-uring as runtime.

use core::future::Future;

use std::{io, net::Shutdown};

pub use tokio_uring::buf::{IoBuf, IoBufMut, Slice};

pub trait AsyncBufRead {
    fn read<B>(&self, buf: B) -> impl Future<Output = (io::Result<usize>, B)>
    where
        B: IoBufMut;
}

pub trait AsyncBufWrite {
    fn write<B>(&self, buf: B) -> impl Future<Output = (io::Result<usize>, B)>
    where
        B: IoBuf;

    fn shutdown(&self, direction: Shutdown) -> io::Result<()>;
}

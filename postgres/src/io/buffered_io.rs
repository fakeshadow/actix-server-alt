use std::{future::pending, io};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use xitca_io::{
    bytes::BytesMut,
    io::{AsyncIo, Interest},
};
use xitca_unsafe_collection::{
    bytes::read_buf,
    futures::{Select as _, SelectOutput},
    uninit,
};

use crate::{
    client::Client,
    error::{unexpected_eof_err, write_zero_err, Error},
    request::Request,
    response::Response,
};

use super::context::Context;

pub struct BufferedIo<Io, const BATCH_LIMIT: usize> {
    io: Io,
    rx: UnboundedReceiver<Request>,
    ctx: Context<BATCH_LIMIT>,
}

impl<Io, const BATCH_LIMIT: usize> BufferedIo<Io, BATCH_LIMIT>
where
    Io: AsyncIo,
{
    pub fn new_pair(io: Io, _: usize) -> (Client, Self) {
        let ctx = Context::<BATCH_LIMIT>::new();

        let (tx, rx) = unbounded_channel();

        (Client::new(tx), Self { io, rx, ctx })
    }

    // send request in self blocking manner. this call would not utilize concurrent read/write nor
    // pipeline/batch. A single response is returned.
    pub async fn linear_request<F, E>(&mut self, encoder: F) -> Result<Response, Error>
    where
        F: FnOnce(&mut BytesMut) -> Result<(), E>,
        Error: From<E>,
    {
        let mut buf = BytesMut::new();
        encoder(&mut buf)?;

        let (req, res) = Request::new_pair(buf);

        self.ctx.push_req(req);

        while !self.ctx.req_is_empty() {
            self.io.ready(Interest::WRITABLE).await?;
            self.try_write()?;
        }

        loop {
            self.io.ready(Interest::READABLE).await?;
            self.try_read()?;

            if self.ctx.try_response_once()? {
                return Ok(res);
            }
        }
    }

    pub fn clear_ctx(&mut self) {
        self.ctx.clear();
    }

    // try read async io until connection error/closed/blocked.
    fn try_read(&mut self) -> Result<(), Error> {
        loop {
            match read_buf(&mut self.io, &mut self.ctx.buf) {
                Ok(0) => return Err(unexpected_eof_err()),
                Ok(_) => continue,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }

    // try write to async io with vectored write enabled.
    fn try_write(&mut self) -> Result<(), Error> {
        loop {
            let mut iovs = uninit::uninit_array::<_, BATCH_LIMIT>();
            let slice = self.ctx.chunks_vectored(&mut iovs);
            match self.io.write_vectored(slice) {
                Ok(0) => return write_zero(self.ctx.req_is_empty()),
                Ok(n) => {
                    self.ctx.advance(n);
                    if self.ctx.req_is_empty() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }

        Ok(())
    }

    pub async fn run(mut self) -> Result<(), Error> {
        loop {
            match try_rx(&mut self.rx, &self.ctx)
                .select(try_io(&mut self.io, &self.ctx))
                .await
            {
                // batch message and keep polling.
                SelectOutput::A(Some(msg)) => self.ctx.push_req(msg),
                // client is gone.
                SelectOutput::A(None) => break,
                SelectOutput::B(ready) => {
                    let ready = ready?;

                    if ready.is_readable() {
                        self.try_read()?;
                        self.ctx.try_response()?;
                    }

                    if ready.is_writable() {
                        self.try_write()?;
                    }
                }
            }
        }

        Ok(())
    }
}

async fn try_rx<const BATCH_LIMIT: usize>(
    rx: &mut UnboundedReceiver<Request>,
    ctx: &Context<BATCH_LIMIT>,
) -> Option<Request> {
    if ctx.req_is_full() {
        pending().await
    } else {
        rx.recv().await
    }
}

fn try_io<'i, Io, const BATCH_LIMIT: usize>(io: &'i mut Io, ctx: &Context<BATCH_LIMIT>) -> Io::ReadyFuture<'i>
where
    Io: AsyncIo,
{
    let interest = if ctx.req_is_empty() {
        Interest::READABLE
    } else {
        Interest::READABLE | Interest::WRITABLE
    };

    io.ready(interest)
}

#[cold]
#[inline(never)]
fn write_zero(is_buf_empty: bool) -> Result<(), Error> {
    assert!(!is_buf_empty, "trying to write from empty buffer.");
    Err(write_zero_err())
}

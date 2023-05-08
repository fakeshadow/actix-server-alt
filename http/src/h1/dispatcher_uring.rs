use core::{
    cell::RefCell,
    fmt,
    future::{poll_fn, Future},
    marker::PhantomData,
    mem,
    pin::{pin, Pin},
    task::{self, ready, Poll, Waker},
};

use std::{
    io,
    net::{Shutdown, SocketAddr},
    rc::Rc,
};

use futures_core::stream::Stream;
use tracing::trace;
use xitca_io::{
    bytes::BytesMut,
    io_uring::{AsyncBufRead, AsyncBufWrite, IoBuf},
};
use xitca_service::Service;
use xitca_unsafe_collection::futures::SelectOutput;

use crate::{
    body::NoneBody,
    bytes::Bytes,
    config::HttpServiceConfig,
    date::DateTime,
    h1::{body::RequestBody, error::Error},
    http::{response::Response, StatusCode},
    util::{
        buffered::ReadBuf,
        timer::{KeepAlive, Timeout},
    },
};

use super::{
    dispatcher::{status_only, Timer},
    proto::{
        codec::{ChunkResult, TransferCoding},
        context::Context,
        encode::encode_continue,
        error::ProtoError,
    },
};

type ExtRequest<B> = crate::http::Request<crate::http::RequestExt<B>>;

/// Http/1 dispatcher
pub(super) struct Dispatcher<'a, Io, S, ReqB, D, const H_LIMIT: usize, const R_LIMIT: usize, const W_LIMIT: usize> {
    io: Rc<Io>,
    timer: Timer<'a>,
    ctx: Context<'a, D, H_LIMIT>,
    service: &'a S,
    read_buf: ReadBuf<R_LIMIT>,
    write_buf: WriteBuf<W_LIMIT>,
    notify: Notify<ReadBufErased>,
    _phantom: PhantomData<ReqB>,
}

struct WriteBuf<const LIMIT: usize> {
    buf: Option<BytesMut>,
}

impl<const LIMIT: usize> WriteBuf<LIMIT> {
    fn new() -> Self {
        Self {
            buf: Some(BytesMut::new()),
        }
    }

    fn get_mut(&mut self) -> &mut BytesMut {
        self.buf
            .as_mut()
            .expect("WriteBuf::write_io is dropped before polling to complete")
    }

    async fn write_io(&mut self, io: &impl AsyncBufWrite) -> io::Result<()> {
        let buf = self
            .buf
            .take()
            .expect("WriteBuf::write_io is dropped before polling to complete");

        let (res, mut buf) = write_all(io, buf).await;

        buf.clear();
        self.buf.replace(buf);

        res
    }
}

async fn write_all(io: &impl AsyncBufWrite, mut buf: BytesMut) -> (io::Result<()>, BytesMut) {
    let mut n = 0;
    while n < buf.bytes_init() {
        match io.write(buf.slice(n..)).await {
            (Ok(0), slice) => {
                return (Err(io::ErrorKind::WriteZero.into()), slice.into_inner());
            }
            (Ok(m), slice) => {
                n += m;
                buf = slice.into_inner();
            }
            (Err(e), slice) => {
                return (Err(e), slice.into_inner());
            }
        }
    }
    (Ok(()), buf)
}

// erase const generic type to ease public type param.
type ReadBufErased = ReadBuf<0>;

impl<const LIMIT: usize> ReadBuf<LIMIT> {
    async fn read_io(&mut self, io: &impl AsyncBufRead) -> io::Result<usize> {
        let mut buf = mem::take(self).into_inner().into_inner();

        let len = buf.len();
        let remaining = buf.capacity() - len;
        if remaining < 4096 {
            buf.reserve(4096 - remaining);
        }

        let (res, buf) = io.read(buf.slice(len..)).await;
        *self = Self::from(buf.into_inner());
        res
    }
}

impl<'a, Io, S, ReqB, ResB, BE, D, const H_LIMIT: usize, const R_LIMIT: usize, const W_LIMIT: usize>
    Dispatcher<'a, Io, S, ReqB, D, H_LIMIT, R_LIMIT, W_LIMIT>
where
    Io: AsyncBufRead + AsyncBufWrite + 'static,
    S: Service<ExtRequest<ReqB>, Response = Response<ResB>>,
    ReqB: From<RequestBody>,
    ResB: Stream<Item = Result<Bytes, BE>>,
    D: DateTime,
{
    pub(super) fn new(
        io: Io,
        addr: SocketAddr,
        timer: Pin<&'a mut KeepAlive>,
        config: HttpServiceConfig<H_LIMIT, R_LIMIT, W_LIMIT>,
        service: &'a S,
        date: &'a D,
    ) -> Self {
        Self {
            io: Rc::new(io),
            timer: Timer::new(timer, config.keep_alive_timeout, config.request_head_timeout),
            ctx: Context::<_, H_LIMIT>::with_addr(addr, date),
            service,
            read_buf: ReadBuf::<R_LIMIT>::new(),
            write_buf: WriteBuf::<W_LIMIT>::new(),
            notify: Notify::new(),
            _phantom: PhantomData,
        }
    }

    pub(super) async fn run(mut self) -> Result<(), Error<S::Error, BE>> {
        loop {
            match self._run().await {
                Ok(_) => {}
                Err(Error::KeepAliveExpire) => {
                    trace!(target: "h1_dispatcher", "Connection keep-alive expired. Shutting down");
                    return Ok(());
                }
                Err(Error::RequestTimeout) => self.request_error(|| status_only(StatusCode::REQUEST_TIMEOUT)),
                Err(Error::Proto(ProtoError::HeaderTooLarge)) => {
                    self.request_error(|| status_only(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE))
                }
                Err(Error::Proto(_)) => self.request_error(|| status_only(StatusCode::BAD_REQUEST)),
                Err(e) => return Err(e),
            }

            self.write_buf.write_io(&*self.io).await?;

            if self.ctx.is_connection_closed() {
                return self.io.shutdown(Shutdown::Both).map_err(Into::into);
            }
        }
    }

    async fn _run(&mut self) -> Result<(), Error<S::Error, BE>> {
        self.timer.update(self.ctx.date().now());

        let read = self
            .read_buf
            .read_io(&*self.io)
            .timeout(self.timer.get())
            .await
            .map_err(|_| self.timer.map_to_err())??;

        if read == 0 {
            self.ctx.set_close();
            return Ok(());
        }

        while let Some((req, decoder)) = self.ctx.decode_head::<R_LIMIT>(&mut self.read_buf)? {
            self.timer.reset_state();

            let (waiter, body) = if decoder.is_eof() {
                (None, RequestBody::default())
            } else {
                let body = Body::new(
                    self.io.clone(),
                    self.ctx.is_expect_header(),
                    R_LIMIT,
                    decoder,
                    mem::take(&mut self.read_buf).limit(),
                    self.notify.notifier(),
                );

                (Some(&mut self.notify), RequestBody::io_uring(body))
            };

            let req = req.map(|ext| ext.map_body(|_| ReqB::from(body)));

            let (parts, body) = self.service.call(req).await.map_err(Error::Service)?.into_parts();

            let mut encoder = self.ctx.encode_head(parts, &body, self.write_buf.get_mut())?;

            // this block is necessary. ResB has to be dropped asap as it may hold ownership of
            // Body type which if not dropped before Notifier::notify is called would prevent
            // Notifier from waking up Notify.
            {
                let mut body = pin!(body);

                loop {
                    let buf = self.write_buf.get_mut();

                    if buf.len() < W_LIMIT {
                        let res = poll_fn(|cx| match body.as_mut().poll_next(cx) {
                            Poll::Ready(res) => Poll::Ready(SelectOutput::A(res)),
                            Poll::Pending if buf.is_empty() => Poll::Pending,
                            Poll::Pending => Poll::Ready(SelectOutput::B(())),
                        })
                        .await;

                        match res {
                            SelectOutput::A(Some(res)) => {
                                let bytes = res.map_err(Error::Body)?;
                                encoder.encode(bytes, buf);
                                continue;
                            }
                            SelectOutput::A(None) => {
                                encoder.encode_eof(buf);
                                break;
                            }
                            SelectOutput::B(_) => {}
                        }
                    }

                    self.write_buf.write_io(&*self.io).await?;
                }
            }

            if let Some(waiter) = waiter {
                match waiter.wait().await {
                    Some(read_buf) => {
                        let _ = mem::replace(&mut self.read_buf, read_buf.limit());
                    }
                    None => {
                        self.ctx.set_close();
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn request_error(&mut self, func: impl FnOnce() -> Response<NoneBody<Bytes>>) {
        self.ctx.set_close();
        let (parts, body) = func().into_parts();
        self.ctx
            .encode_head(parts, &body, self.write_buf.get_mut())
            .expect("request_error must be correct");
    }
}

pub(super) struct Body(Pin<Box<dyn Stream<Item = io::Result<Bytes>>>>);

impl Body {
    fn new<Io>(
        io: Rc<Io>,
        is_expect: bool,
        limit: usize,
        decoder: TransferCoding,
        read_buf: ReadBufErased,
        notify: Notifier<ReadBufErased>,
    ) -> Self
    where
        Io: AsyncBufRead + AsyncBufWrite + 'static,
    {
        let body = _Body {
            io,
            limit,
            decoder: Decoder {
                decoder,
                read_buf,
                notify,
            },
        };

        let body = if is_expect {
            BodyState::Future(Box::pin(async {
                let mut bytes = BytesMut::new();
                encode_continue(&mut bytes);
                let (res, _) = write_all(&*body.io, bytes).await;
                res.map(|_| body)
            }))
        } else {
            BodyState::Body(body)
        };

        Self(Box::pin(body))
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Body")
    }
}

impl Clone for Body {
    fn clone(&self) -> Self {
        unimplemented!("rework body module so it does not force Clone on Body type.")
    }
}

impl Stream for Body {
    type Item = io::Result<Bytes>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().0).poll_next(cx)
    }
}

enum BodyState<Io> {
    Body(_Body<Io>),
    Future(Pin<Box<dyn Future<Output = io::Result<_Body<Io>>>>>),
    None,
}

struct _Body<Io> {
    io: Rc<Io>,
    limit: usize,
    decoder: Decoder,
}

impl<Io> Stream for BodyState<Io>
where
    Io: AsyncBufRead + 'static,
{
    type Item = io::Result<Bytes>;

    #[inline]
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.as_mut().get_mut() {
                Self::Body(body) => {
                    match body.decoder.decoder.decode(&mut body.decoder.read_buf) {
                        ChunkResult::Ok(bytes) => return Poll::Ready(Some(Ok(bytes))),
                        ChunkResult::Err(e) => return Poll::Ready(Some(Err(e))),
                        ChunkResult::InsufficientData => {}
                        _ => return Poll::Ready(None),
                    }

                    if body.decoder.read_buf.len() >= body.limit {
                        let msg = format!(
                            "READ_BUF_LIMIT reached: {{ limit: {}, length: {} }}",
                            body.limit,
                            body.decoder.read_buf.len()
                        );
                        return Poll::Ready(Some(Err(io::Error::new(io::ErrorKind::Other, msg))));
                    }

                    let Self::Body(mut body) = mem::replace(self.as_mut().get_mut(), Self::None) else { unreachable!() };

                    self.as_mut().set(Self::Future(Box::pin(async {
                        let read = body.decoder.read_buf.read_io(&*body.io).await?;
                        if read == 0 {
                            return Err(io::ErrorKind::UnexpectedEof.into());
                        }
                        Ok(body)
                    })))
                }
                Self::Future(fut) => {
                    let body = ready!(Pin::new(fut).poll(cx))?;
                    self.as_mut().set(Self::Body(body));
                }
                Self::None => unreachable!(
                    "None variant is only used internally and must not be observable from stream consumer."
                ),
            }
        }
    }
}

struct Decoder {
    decoder: TransferCoding,
    read_buf: ReadBufErased,
    notify: Notifier<ReadBufErased>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if self.decoder.is_eof() {
            let buf = mem::take(&mut self.read_buf);
            self.notify.notify(buf);
        }
    }
}

struct Notify<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

impl<T> Notify<T> {
    fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner { waker: None, val: None })),
        }
    }

    fn notifier(&mut self) -> Notifier<T> {
        Notifier(self.inner.clone())
    }

    async fn wait(&mut self) -> Option<T> {
        poll_fn(|cx| {
            let strong_count = Rc::strong_count(&self.inner);
            let mut inner = self.inner.borrow_mut();
            if let Some(val) = inner.val.take() {
                return Poll::Ready(Some(val));
            } else if strong_count == 1 {
                return Poll::Ready(None);
            }
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

struct Notifier<T>(Rc<RefCell<Inner<T>>>);

impl<T> Drop for Notifier<T> {
    fn drop(&mut self) {
        if let Some(waker) = self.0.borrow_mut().waker.take() {
            waker.wake();
        }
    }
}

impl<T> Notifier<T> {
    fn notify(&mut self, val: T) {
        self.0.borrow_mut().val = Some(val);
    }
}

struct Inner<V> {
    waker: Option<Waker>,
    val: Option<V>,
}

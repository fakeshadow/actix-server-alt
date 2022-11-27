#![allow(dead_code)]

mod dispatcher;
mod head;
mod hpack;
mod settings;
mod stream_id;

pub(crate) use dispatcher::Dispatcher;

pub const HEADER_LEN: usize = 9;

use std::io::{self};

use xitca_io::{
    bytes::{Buf, Bytes, BytesMut},
    io::AsyncIo,
};

use crate::util::buffered_io::{self, BufWrite, ListBuf};

use self::settings::Settings;

const PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

type BufferedIo<'i, Io, W> = buffered_io::BufferedIo<'i, Io, W, { 1024 * 1024 }>;

pub async fn run<Io>(mut io: Io) -> io::Result<()>
where
    Io: AsyncIo,
{
    let write_buf = ListBuf::<Bytes, 32>::default();
    let mut io = BufferedIo::new(&mut io, write_buf);

    let settings = Settings::default();

    let mut buf = BytesMut::new();

    settings.encode(&mut buf);

    io.write_buf.buffer(buf.freeze());

    io.drain_write().await?;

    loop {
        io.read().await?;
        if io.read_buf.len() >= PREFACE.len() {
            if &io.read_buf[..PREFACE.len()] == PREFACE {
                io.read_buf.advance(PREFACE.len());
                break;
            } else {
                todo!()
            }
        }
    }

    let mut _decoder = hpack::Decoder::new(settings::DEFAULT_SETTINGS_HEADER_TABLE_SIZE);

    {
        let frame = recv_frame(&mut io).await?;

        let head = head::Head::parse(&frame);

        // settings ack is ignored for now.
        assert_eq!(head.kind(), head::Kind::Settings);
        assert_eq!(head.flag(), 0);
        assert!(head.stream_id().is_zero());
    }

    // naively assume a header frame is gonna come in.
    let frame = recv_frame(&mut io).await?;
    let head = head::Head::parse(&frame);

    assert_eq!(head.kind(), head::Kind::Headers);

    Ok(())
}

async fn recv_frame<Io, W>(io: &mut BufferedIo<'_, Io, W>) -> io::Result<BytesMut>
where
    Io: AsyncIo,
    W: BufWrite,
{
    while io.read_buf.len() < 3 {
        io.read().await?;
    }

    let len = (io.read_buf.get_uint(3) + 6) as usize;

    while io.read_buf.len() < len {
        io.read().await?;
    }

    Ok(io.read_buf.split_to(len))
}

/// A helper macro that unpacks a sequence of 4 bytes found in the buffer with
/// the given identifier, starting at the given offset, into the given integer
/// type. Obviously, the integer type should be able to support at least 4
/// bytes.
///
/// # Examples
///
/// ```ignore
/// # // We ignore this doctest because the macro is not exported.
/// let buf: [u8; 4] = [0, 0, 0, 1];
/// assert_eq!(1u32, unpack_octets_4!(buf, 0, u32));
/// ```
macro_rules! unpack_octets_4 {
    // TODO: Get rid of this macro
    ($buf:expr, $offset:expr, $tip:ty) => {
        (($buf[$offset + 0] as $tip) << 24)
            | (($buf[$offset + 1] as $tip) << 16)
            | (($buf[$offset + 2] as $tip) << 8)
            | (($buf[$offset + 3] as $tip) << 0)
    };
}

use unpack_octets_4;

#[cfg(test)]
mod tests {
    #[test]
    fn test_unpack_octets_4() {
        let buf: [u8; 4] = [0, 0, 0, 1];
        assert_eq!(1u32, unpack_octets_4!(buf, 0, u32));
    }
}

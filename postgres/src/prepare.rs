use std::sync::atomic::{AtomicUsize, Ordering};

use postgres_protocol::message::frontend;
use postgres_types::Type;
use tokio::sync::mpsc::channel;
use tracing::debug;
use xitca_io::bytes::{Bytes, BytesMut};

use super::{client::Client, error::Error, message::Message};

impl Client {
    pub async fn prepare(&self, query: &str, types: &[Type]) -> Result<(), Error> {
        let buf = prepare_buf(&mut *self.buf.borrow_mut(), query, types)?;

        let (tx, rx) = channel::<()>(1);

        self.tx.send(Message(tx)).await?;

        Ok(())
    }
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn prepare_buf(buf: &mut BytesMut, query: &str, types: &[Type]) -> Result<Bytes, Error> {
    let name = &format!("s{}", NEXT_ID.fetch_add(1, Ordering::SeqCst));

    if types.is_empty() {
        debug!("preparing query {}: {}", name, query);
    } else {
        debug!("preparing query {} with types {:?}: {}", name, types, query);
    }

    frontend::parse(name, query, types.iter().map(Type::oid), buf)?;
    frontend::describe(b'S', name, buf)?;
    frontend::sync(buf);

    Ok(buf.split().freeze())
}

// fn concurrent(c: &mut Client, query: &str, types: &[Type]) {
//     let mut v = Vec::new();

//     for _ in 0..32 {
//         let prepare = c.prepare(query, types);

//         v.push(prepare.run())
//     }
// }

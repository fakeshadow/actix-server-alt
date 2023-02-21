use core::{
    pin::Pin,
    task::{ready, Context, Poll},
};

use std::sync::Arc;

use fallible_iterator::FallibleIterator;
use futures_core::stream::Stream;
use postgres_protocol::message::{backend, frontend};

use crate::{client::Client, column::Column, error::Error, response::Response, row::RowSimple, Type};

impl Client {
    pub fn query_simple(&self, stmt: &str) -> Result<RowSimpleStream, Error> {
        self.simple(stmt).map(|res| RowSimpleStream { res, columns: None })
    }

    pub async fn execute_simple(&self, stmt: &str) -> Result<u64, Error> {
        let mut res = self.simple(stmt)?;
        let mut rows = 0;
        loop {
            match res.recv().await? {
                backend::Message::RowDescription(_) | backend::Message::DataRow(_) => {}
                backend::Message::CommandComplete(body) => {
                    rows = super::base::extract_row_affected(&body)?;
                }
                backend::Message::EmptyQueryResponse => rows = 0,
                backend::Message::ReadyForQuery(_) => return Ok(rows),
                _ => return Err(Error::UnexpectedMessage),
            }
        }
    }

    fn simple(&self, stmt: &str) -> Result<Response, Error> {
        let buf = self.with_buf_fallible(|buf| frontend::query(stmt, buf).map(|_| buf.split()))?;
        self.send(buf)
    }
}

/// A stream of simple query results.
pub struct RowSimpleStream {
    res: Response,
    // TODO: GAT async iterator for &'a [Column]
    columns: Option<Arc<[Column]>>,
}

impl Stream for RowSimpleStream {
    type Item = Result<RowSimple, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match ready!(this.res.poll_recv(cx)?) {
                backend::Message::RowDescription(body) => {
                    let columns = body
                        .fields()
                        .map(|f| Ok(Column::new(f.name(), Type::ANY)))
                        .collect::<Vec<_>>()?
                        .into();
                    this.columns = Some(columns);
                }
                backend::Message::DataRow(body) => {
                    let res = this
                        .columns
                        .as_ref()
                        .ok_or(Error::UnexpectedMessage)
                        .and_then(|col| RowSimple::try_new(col.clone(), body));
                    return Poll::Ready(Some(res));
                }
                backend::Message::CommandComplete(_)
                | backend::Message::EmptyQueryResponse
                | backend::Message::ReadyForQuery(_) => return Poll::Ready(None),
                _ => return Poll::Ready(Some(Err(Error::UnexpectedMessage))),
            }
        }
    }
}

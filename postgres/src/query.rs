mod stream;

#[cfg(feature = "compat")]
pub(crate) mod compat;

pub use stream::{RowSimpleStream, RowStream, RowStreamGuarded};

use core::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

use super::{
    driver::codec::{AsParams, Encode, IntoStream, Response},
    error::Error,
    iter::slice_iter,
    types::ToSql,
};

/// trait generic over api used for querying with typed prepared statement.
///
/// types like [Transaction] and [CopyIn] accept generic client type and they are able to use user supplied
/// client new type to operate and therefore reduce less new types and methods boilerplate.
///
/// [Transaction]: crate::transaction::Transaction
/// [CopyIn]: crate::copy::CopyIn
pub trait Query {
    /// query with statement and dynamic typed params.
    ///
    /// statement must be a type impl [`Encode`] trait and there are currently 3 major types available:
    ///
    /// # [`Statement`] type category
    /// This category includes multiple types that can be dereferenced/borrowed as [`Statement`]
    /// ## Examples
    /// ```rust
    /// # use xitca_postgres::{dev::{Prepare, Query}, types::Type, AsyncLendingIterator, Client, Error};
    /// # async fn prepare_and_query(client: &Client) -> Result<(), Error> {
    /// // prepare a statement with client type.
    /// let stmt = client._prepare("SELECT id from users", &[Type::INT4]).await?;
    /// // query with statement and typed params for a stream of rows
    /// let mut stream = client._query(&stmt, &[&996i32])?;
    /// // obtain the first row and get user id.
    /// let row = stream.try_next().await?.unwrap();      
    /// let _id: i32 = row.try_get("id")?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # [`StatementUnnamed`] type category
    /// This category is for embedding prepare statement to the query request itself. Meaning query would finish
    /// in one round trip to database. However it should also noted that the client type must be referenced during
    /// the whole progress and associated client must be kept around util streaming is finished.
    /// ## Examples
    /// ```rust
    /// # use xitca_postgres::{dev::{Prepare, Query}, statement::Statement, types::Type, AsyncLendingIterator, Client, Error};
    /// # async fn prepare_and_query(client: &Client) -> Result<(), Error> {
    /// // construct an unnamed statement.
    /// let stmt = Statement::unnamed(client, "SELECT id from users", &[Type::INT4]);
    /// // query with the unnamed statement.
    /// // under the hood the statement is prepared in background and used for query and stream row parsing
    /// let mut stream = client._query(stmt, &[&996i32])?;
    /// // obtain the first row and get user id.
    /// let row = stream.try_next().await?.unwrap();      
    /// let _id: i32 = row.try_get("id")?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # [`str`] type
    /// This category includes multiple types that can be dereferenced/borrowed as [`str`]
    /// ## Examples
    /// ```rust
    /// # use xitca_postgres::{dev::{Prepare, Query}, statement::Statement, types::Type, AsyncLendingIterator, Client, Error};
    /// # async fn simple_query(client: &Client) -> Result<(), Error> {
    /// // query with a string. the string can contain multiple sql query and they have to be separated by semicolons
    /// let mut stream = client._query("SELECT 1;SELECT 1", &[])?;
    /// let _ = stream.try_next().await?;      
    /// # Ok(())
    /// # }
    /// ```
    /// [`Statement`]: crate::statement::Statement
    /// [`StatementUnnamed`]: crate::statement::StatementUnnamed
    #[inline]
    fn _query<'a, S>(
        &self,
        stmt: S,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<<S::Output<'_> as IntoStream>::RowStream<'a>, Error>
    where
        S: Encode + 'a,
    {
        self._query_raw(stmt, slice_iter(params))
    }

    /// flexible version of [Query::_query]
    #[inline]
    fn _query_raw<'a, S, I>(&self, stmt: S, params: I) -> Result<<S::Output<'_> as IntoStream>::RowStream<'a>, Error>
    where
        S: Encode + 'a,
        I: AsParams,
    {
        self._send_encode_query(stmt, params)
            .map(|(stream, res)| stream.into_stream(res))
    }

    /// query that don't return any row but number of rows affected by it
    #[inline]
    fn _execute<S>(&self, stmt: S, params: &[&(dyn ToSql + Sync)]) -> ExecuteFuture
    where
        S: Encode,
    {
        self._execute_raw(stmt, slice_iter(params))
    }

    /// flexible version of [Query::_execute]
    fn _execute_raw<S, I>(&self, stmt: S, params: I) -> ExecuteFuture
    where
        S: Encode,
        I: AsParams,
    {
        let res = self._send_encode_query(stmt, params).map(|(_, res)| res).map_err(Some);
        // TODO:
        // use async { res?.try_into_row_affected().await } with Rust 2024 edition
        ExecuteFuture { res, rows_affected: 0 }
    }

    /// encode statement and params and send it to client driver
    fn _send_encode_query<S, I>(&self, stmt: S, params: I) -> Result<(S::Output<'_>, Response), Error>
    where
        S: Encode,
        I: AsParams;
}

pub struct ExecuteFuture {
    res: Result<Response, Option<Error>>,
    rows_affected: u64,
}

impl Future for ExecuteFuture {
    type Output = Result<u64, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this.res {
            Ok(ref mut res) => {
                ready!(res.poll_try_into_ready(&mut this.rows_affected, cx))?;
                Poll::Ready(Ok(this.rows_affected))
            }
            Err(ref mut e) => Poll::Ready(Err(e.take().expect("ExecuteFuture polled after finish"))),
        }
    }
}

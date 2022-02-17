mod and_then;
mod function;
mod map;
mod map_err;
mod pipeline;
mod transform_fn;

use core::{future::Future, ops::Deref, pin::Pin};

use alloc::{boxed::Box, rc::Rc, sync::Arc};

use super::service::Service;

/// Extend trait for [Service].
///
/// Can be used to cehck the ready state of a service before calling it.
///
/// # Examples:
/// ```rust
/// #![feature(generic_associated_types, type_alias_impl_trait)]
/// # use std::{cell::Cell, rc::Rc, future::Future};
/// # use xitca_service::{Service, ready::ReadyService};
///
/// // a service with conditional avalibility based on state of Permit.
/// struct Foo(Permit);
///
/// // a permit reset the inner boolean to true on drop.
/// #[derive(Clone)]
/// struct Permit(Rc<Cell<bool>>);
///
/// impl Drop for Permit {
///     fn drop(&mut self) {
///         self.0.set(true);
///     }
/// }
///
/// impl Service<()> for Foo {
///     type Response = ();
///     type Error = ();
///     type Future<'f> = impl Future<Output = Result<Self::Response, Self::Error>>;
///
///     fn call(&self, _req: ()) -> Self::Future<'_> {
///         async { Ok(()) }
///     }
/// }
///
/// impl ReadyService<()> for Foo {
///     type Ready = Permit;
///     type ReadyFuture<'f> = impl Future<Output = Result<Self::Ready, Self::Error>>;
///
///     fn ready(&self) -> Self::ReadyFuture<'_> {
///         async move {
///             if self.0.0.get() {
///                 // set permit to false and return with Ok<Permit>
///                 self.0.0.set(false);
///                 Ok(self.0.clone())
///             } else {
///                 // return error is to simply the example.
///                 // In real world this branch should be an async waiting for Permit reset to true.
///                 Err(())
///             }                
///         }
///     }
/// }
///
/// async fn workflow(service: &Foo) {
///     let permit = service.ready().await.unwrap(); // check service ready state.
///
///     service.call(()).await.unwrap(); // run Service::call when permit is held in scope.
///
///     drop(permit); // drop permit after Service::call is finished.
/// }
///
/// async fn throttle(service: &Foo) {
///     let permit = service.ready().await.unwrap();
///     assert!(service.ready().await.is_err());  // service is throttled because permit is still helded in scope.
/// }
/// ```
pub trait ReadyService<Req>: Service<Req> {
    type Ready;

    type ReadyFuture<'f>: Future<Output = Result<Self::Ready, Self::Error>>
    where
        Self: 'f;

    fn ready(&self) -> Self::ReadyFuture<'_>;
}

macro_rules! impl_alloc {
    ($alloc: ident) => {
        impl<S, Req> ReadyService<Req> for $alloc<S>
        where
            S: ReadyService<Req> + ?Sized,
        {
            type Ready = S::Ready;
            type ReadyFuture<'f>
            where
                Self: 'f,
            = S::ReadyFuture<'f>;

            #[inline]
            fn ready(&self) -> Self::ReadyFuture<'_> {
                (**self).ready()
            }
        }
    };
}

impl_alloc!(Box);
impl_alloc!(Rc);
impl_alloc!(Arc);

impl<S, Req> ReadyService<Req> for Pin<S>
where
    S: Deref,
    S::Target: ReadyService<Req>,
{
    type Ready = <S::Target as ReadyService<Req>>::Ready;
    type ReadyFuture<'f>
    where
        Self: 'f,
    = <S::Target as ReadyService<Req>>::ReadyFuture<'f>;

    #[inline]
    fn ready(&self) -> Self::ReadyFuture<'_> {
        self.as_ref().get_ref().ready()
    }
}

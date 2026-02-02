/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use crate::client::CallOptions;
use crate::client::Invoke;
use crate::client::RecvStream;
use crate::client::SendStream;

/// A trait which allows intercepting an RPC invoke operation.  The trait is
/// generic on I which should either be implemented as Invoke (for interceptors
/// that only need to call next once) or InvokeFactory (for interceptors that
/// need to call next multiple times).
pub trait Intercept<I>: Send + Sync {
    type SendStream: SendStream + 'static;
    type RecvStream: RecvStream + 'static;

    /// Intercepts the start of a call.  Implementations should generally use
    /// next to create and start a call whose streams are optionally wrapped
    /// before being returned.
    fn intercept(
        self,
        method: String,
        options: CallOptions,
        next: I,
    ) -> (Self::SendStream, Self::RecvStream);
}

/// A trait that is automatically implemented for any types that implement
/// Invoke on their receivers and match the other supertrait bounds.
pub trait InvokeFactory: Send + Sync + Clone + 'static {
    type SendStream: SendStream + 'static;
    type RecvStream: RecvStream + 'static;

    fn invoke(&self, method: String, options: CallOptions) -> (Self::SendStream, Self::RecvStream);
}

impl<T, SS, RS> InvokeFactory for T
where
    T: Send + Sync + Clone + 'static,
    for<'a> &'a T: Invoke<SendStream = SS, RecvStream = RS>,
    SS: SendStream + 'static,
    RS: RecvStream + 'static,
{
    type SendStream = SS;
    type RecvStream = RS;

    fn invoke(&self, method: String, options: CallOptions) -> (Self::SendStream, Self::RecvStream) {
        Invoke::invoke(self, method, options)
    }
}

/// Wraps an interceptor whose referece implements Intercept, and implements
/// Intercept on the owned value instead, allowing it to be paired with a
/// non-reusable Invoke implementation.
#[derive(Clone)]
pub struct IntoOnce<T>(pub T);

impl<I, T: Send + Sync, SS, RS> Intercept<I> for IntoOnce<T>
where
    I: Invoke,
    SS: SendStream + 'static,
    RS: RecvStream + 'static,
    for<'a> &'a T: Intercept<I, SendStream = SS, RecvStream = RS>,
{
    type SendStream = SS;
    type RecvStream = RS;

    fn intercept(
        self,
        method: String,
        options: CallOptions,
        next: I,
    ) -> (Self::SendStream, Self::RecvStream) {
        (&self.0).intercept(method, options, next)
    }
}

/// Wraps `Invoke` and an `Intercept` impls and implements `Invoke` for the
/// combination, either on its value if the combination may only be used once,
/// or on its reference if the combination is reusable -- i.e. `Inv` is an
/// `InvokeFactory` and Int is `Intercept<InvokeFactory>`.
#[derive(Clone, Copy)]
pub struct Intercepted<Inv, Int> {
    invoke: Inv,
    intercept: Int,
}

impl<Inv, Int> Intercepted<Inv, Int> {
    pub fn new(invoke: Inv, intercept: Int) -> Self {
        Self { invoke, intercept }
    }
}

impl<Inv, Int> Invoke for Intercepted<Inv, Int>
where
    Inv: Invoke,
    Int: Intercept<Inv>,
{
    type SendStream = Int::SendStream;
    type RecvStream = Int::RecvStream;

    fn invoke(self, method: String, options: CallOptions) -> (Self::SendStream, Self::RecvStream) {
        self.intercept.intercept(method, options, self.invoke)
    }
}

impl<'a, Inv, Int> Invoke for &'a Intercepted<Inv, Int>
where
    Inv: Send + Sync,
    Int: Send + Sync,
    &'a Int: Intercept<&'a Inv>,
{
    type SendStream = <&'a Int as Intercept<&'a Inv>>::SendStream;
    type RecvStream = <&'a Int as Intercept<&'a Inv>>::RecvStream;

    fn invoke(self, method: String, options: CallOptions) -> (Self::SendStream, Self::RecvStream) {
        // Delegate to the reference
        (&self.intercept).intercept(method, options, &self.invoke)
    }
}

/// Combines an `InvokeFactory` with a single-use `Intercept<InvokeFactory>` to
/// implement `Invoke`.
pub struct InterceptedOnce<Inv, Int> {
    invoke: Inv,
    intercept: Int,
}

impl<Inv, Int> InterceptedOnce<Inv, Int> {
    pub fn new(invoke: Inv, intercept: Int) -> Self {
        Self { invoke, intercept }
    }
}

impl<Inv, Int, SS, RS> Invoke for InterceptedOnce<Inv, Int>
where
    Inv: InvokeFactory,
    for<'a> Int: Intercept<&'a Inv, SendStream = SS, RecvStream = RS>,
    SS: SendStream + 'static,
    RS: RecvStream + 'static,
{
    type SendStream = SS;
    type RecvStream = RS;

    fn invoke(self, method: String, options: CallOptions) -> (Self::SendStream, Self::RecvStream) {
        self.intercept.intercept(method, options, &self.invoke)
    }
}

/// Implements methods for combining InvokeFactory implementations with
/// interceptors.  Blanket implemented on any InvokeFactory.
pub trait InvokeFactoryExt: Sized
where
    for<'a> &'a Self: Invoke,
{
    fn with_interceptor<Int>(self, interceptor: Int) -> Intercepted<Self, Int>
    where
        for<'a> &'a Int: Intercept<&'a Self>,
    {
        Intercepted::new(self, interceptor)
    }

    fn with_once_interceptor<Int>(self, interceptor: Int) -> InterceptedOnce<Self, Int>
    where
        for<'a> Int: Intercept<&'a Self>,
    {
        InterceptedOnce::new(self, interceptor)
    }
}

/// Implements methods for combining Invoke implementations with interceptors.
/// Blanket implemented on any Invoke.
pub trait InvokeExt: Invoke + Sized {
    fn with_interceptor<Int>(self, interceptor: Int) -> Intercepted<Self, IntoOnce<Int>>
    where
        for<'a> &'a Int: Intercept<Self>,
    {
        Intercepted::new(self, IntoOnce(interceptor))
    }

    fn with_once_interceptor<Int>(self, interceptor: Int) -> Intercepted<Self, Int>
    where
        Int: Intercept<Self>,
    {
        Intercepted::new(self, interceptor)
    }
}

impl<T: Sized> InvokeFactoryExt for T where for<'a> &'a Self: Invoke {}
impl<T: Invoke + Sized> InvokeExt for T {}

// Tests for Intercepted and InterceptedOnce and examples of the four types of
// interceptors, how they are implemented, and how they are combined with
// Invoke/InvokeOnce.
//
// The four different kinds of interceptors are:
//
// 1. Reusable: uses `next` once.
//    impl Intercept<Invoke> on &MyInterceptor
//
// 2. ResuableFanOut: uses `next` multiple times.
//    impl Intercept<&InvokeFactory> on &MyInterceptor
//
// 3. Oneshot: uses `next` once.
//    impl InterceptOnce<Invoke> on MyInterceptor
//
// 4. OneshotFanOut: uses `next` multiple times.
//    impl InterceptOnce<&InvokeFactory> on MyInterceptor
//
// Examples of each of these are defined below.
#[cfg(test)]
mod test {
    use super::*;
    use crate::client::CallOptions;
    use crate::client::Invoke;
    use crate::client::RecvStream;
    use crate::client::SendOptions;
    use crate::client::SendStream;
    use crate::core::ClientResponseStreamItem;
    use crate::core::RecvMessage;
    use crate::core::ResponseHeaders;
    use crate::core::SendMessage;
    use crate::core::Trailers;
    use crate::Status;
    use crate::StatusCode;
    use bytes::Bytes;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::Arc;
    use tokio::pin;
    use tokio::select;
    use tokio::sync::broadcast;
    use tokio::sync::mpsc;
    use tokio::sync::Mutex;
    use tokio::sync::Notify;
    use tokio::task;

    struct Reusable;
    impl<I: Invoke> Intercept<I> for &Reusable {
        type SendStream = NopStream;
        type RecvStream = I::RecvStream;

        fn intercept(
            self,
            method: String,
            args: CallOptions,
            next: I,
        ) -> (Self::SendStream, Self::RecvStream) {
            let (_, rx) = next.invoke(method, args);
            (NopStream, rx)
        }
    }

    struct ReusableFanOut;
    impl<I: InvokeFactory> Intercept<&I> for &ReusableFanOut {
        type SendStream = RetrySendStream<I::SendStream>;
        type RecvStream = RetryRecvStream<I>;

        fn intercept(
            self,
            method: String,
            args: CallOptions,
            next: &I,
        ) -> (Self::SendStream, Self::RecvStream) {
            start_retry_streams(next, method, args)
        }
    }
    struct Oneshot;
    impl<I: Invoke> Intercept<I> for Oneshot {
        type SendStream = I::SendStream;
        type RecvStream = NopStream;

        fn intercept(
            self,
            method: String,
            args: CallOptions,
            next: I,
        ) -> (Self::SendStream, Self::RecvStream) {
            let (tx, _) = next.invoke(method, args);
            (tx, NopStream)
        }
    }

    struct OneshotFanOut;
    impl<I: InvokeFactory> Intercept<&I> for OneshotFanOut {
        type SendStream = I::SendStream;
        type RecvStream = I::RecvStream;

        fn intercept(
            self,
            method: String,
            args: CallOptions,
            next: &I,
        ) -> (Self::SendStream, Self::RecvStream) {
            let (_, _) = next.invoke(method.clone(), args.clone());
            next.invoke(method, args)
        }
    }

    // Tests all 6 valid scenarios combining an invoker with an interceptor.
    #[test]
    fn test_interceptor_creation() {
        // Reusable Invoke with resuable Intercept.
        {
            let i = NopInvoker.with_interceptor(Reusable);
            i.invoke("".to_string(), CallOptions::default());
            // Since Invoke is implemented on &Intercepted, it is reusable.
            i.invoke("".to_string(), CallOptions::default());
        }

        // One-shot Invoke with resuable Intercept.
        {
            let i = NopOnceInvoker.with_interceptor(Reusable);
            i.invoke("".to_string(), CallOptions::default());
        }

        // Reusable Invoke with resuable fan-out Intercept.
        {
            let i = NopInvoker.with_interceptor(ReusableFanOut);
            i.invoke("".to_string(), CallOptions::default());
            // Since Invoke is implemented on &Intercepted, it is reusable.
            i.invoke("".to_string(), CallOptions::default());
        }

        // One-shot Invoke with fan-out Intercept is illegal.

        // Reusable Invoke with one-shot Intercept.
        {
            let i = NopInvoker.with_once_interceptor(Oneshot);
            i.invoke("".to_string(), CallOptions::default());
        }

        // One-shot Invoke with one-shot Intercept.
        {
            let i = NopOnceInvoker.with_once_interceptor(Oneshot);
            i.invoke("".to_string(), CallOptions::default());
        }

        // Reusable Invoke with one-shot fan-out Intercept.
        {
            let i = NopInvoker.with_once_interceptor(OneshotFanOut);
            i.invoke("".to_string(), CallOptions::default());
        }

        // One-shot Invoke with one-shot fan-out Intercept is illegal.
    }

    // Tests that a a retry interceptor retries cached send operations and
    // ultimately completes with success on an OK response.
    #[tokio::test]
    async fn test_retry_interceptor_succeeds() {
        let (invoker, mut controller) = MockInvoker::new();
        let chan = invoker.with_interceptor(ReusableFanOut);
        let (mut tx, mut rx) = chan.invoke("".to_string(), CallOptions::default());
        let one = VecDeque::from(vec![Bytes::from(vec![1])]);
        let two = VecDeque::from(vec![Bytes::from(vec![2])]);
        tx.send(&ByteSendMsg::new(&one), SendOptions::default())
            .await
            .unwrap();
        assert_eq!(controller.recv_req().await.0, one);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(StatusCode::Internal, ""),
            }))
            .await;
        let handle = task::spawn(async move { rx.next(&mut ByteRecvMsg::new()).await });
        assert_eq!(controller.recv_req().await.0, one);
        tx.send(&ByteSendMsg::new(&two), SendOptions::default())
            .await
            .unwrap();
        assert_eq!(controller.recv_req().await.0, two);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(StatusCode::Internal, ""),
            }))
            .await;
        assert_eq!(controller.recv_req().await.0, one);
        assert_eq!(controller.recv_req().await.0, two);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(StatusCode::Ok, ""),
            }))
            .await;
        let resp = handle.await.unwrap();
        let ClientResponseStreamItem::Trailers(trailers) = resp else {
            panic!("unexpected resp: {resp:?}");
        };
        assert_eq!(trailers.status.code(), StatusCode::Ok);
    }

    // Tests that a a retry interceptor retries cached send operations but fails
    // after the backing streams fail three times.
    #[tokio::test]
    async fn test_retry_interceptor_fails() {
        let (invoker, mut controller) = MockInvoker::new();
        let chan = invoker.with_interceptor(ReusableFanOut);
        let (mut tx, mut rx) = chan.invoke("".to_string(), CallOptions::default());
        let one = VecDeque::from(vec![Bytes::from(vec![1])]);
        let two = VecDeque::from(vec![Bytes::from(vec![2])]);
        tx.send(&ByteSendMsg::new(&one), SendOptions::default())
            .await
            .unwrap();
        assert_eq!(controller.recv_req().await.0, one);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(crate::StatusCode::Internal, ""),
            }))
            .await;
        let handle = task::spawn(async move { rx.next(&mut ByteRecvMsg::new()).await });
        assert_eq!(controller.recv_req().await.0, one);
        tx.send(&ByteSendMsg::new(&two), SendOptions::default())
            .await
            .unwrap();
        assert_eq!(controller.recv_req().await.0, two);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(crate::StatusCode::Internal, ""),
            }))
            .await;
        assert_eq!(controller.recv_req().await.0, one);
        assert_eq!(controller.recv_req().await.0, two);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(crate::StatusCode::Internal, ""),
            }))
            .await;
        assert_eq!(controller.recv_req().await.0, one);
        assert_eq!(controller.recv_req().await.0, two);
        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(crate::StatusCode::Internal, ""),
            }))
            .await;
        let resp = handle.await.unwrap();
        let ClientResponseStreamItem::Trailers(trailers) = resp else {
            panic!("unexpected resp: {resp:?}");
        };
        assert_eq!(trailers.status.code(), crate::StatusCode::Internal);
    }

    // Tests that a a retry interceptor doesn't retry cached operations after
    // receiving the headers from the server.
    #[tokio::test]
    async fn test_retry_interceptor_commit_on_headers() {
        let (invoker, mut controller) = MockInvoker::new();
        let chan = invoker.with_interceptor(ReusableFanOut);
        let (mut tx, mut rx) = chan.invoke("".to_string(), CallOptions::default());
        let one = VecDeque::from(vec![Bytes::from(vec![1])]);
        tx.send(&ByteSendMsg::new(&one), SendOptions::default())
            .await
            .unwrap();
        assert_eq!(controller.recv_req().await.0, one);
        controller
            .send_resp(ClientResponseStreamItem::Headers(ResponseHeaders {}))
            .await;

        let resp = rx.next(&mut ByteRecvMsg::new()).await;
        assert!(matches!(resp, ClientResponseStreamItem::Headers(_)));

        controller
            .send_resp(ClientResponseStreamItem::Trailers(Trailers {
                status: Status::new(crate::StatusCode::Internal, ""),
            }))
            .await;

        let resp = rx.next(&mut ByteRecvMsg::new()).await;
        let ClientResponseStreamItem::Trailers(trailers) = resp else {
            panic!("unexpected resp: {resp:?}");
        };
        assert_eq!(trailers.status.code(), crate::StatusCode::Internal);
    }

    /// An Invoke impl that can be controlled via its paired
    /// MockInvokerController.
    #[derive(Clone)]
    struct MockInvoker {
        resp_tx: broadcast::Sender<ClientResponseStreamItem>,
        req_tx: mpsc::Sender<(VecDeque<Bytes>, SendOptions)>,
    }
    /// A controller used to control the behavior of its paired MockInvoker's
    /// SendStream and RecvStream.
    struct MockInvokerController {
        resp_tx: broadcast::Sender<ClientResponseStreamItem>,
        req_rx: mpsc::Receiver<(VecDeque<Bytes>, SendOptions)>,
    }
    impl MockInvoker {
        fn new() -> (Self, MockInvokerController) {
            // We create receivers as needed in invoke().
            let (resp_tx, _) = broadcast::channel(1);
            let (req_tx, req_rx) = mpsc::channel(1);

            (
                MockInvoker {
                    resp_tx: resp_tx.clone(),
                    req_tx,
                },
                MockInvokerController { req_rx, resp_tx },
            )
        }
    }

    impl MockInvokerController {
        async fn recv_req(&mut self) -> (VecDeque<Bytes>, SendOptions) {
            self.req_rx.recv().await.unwrap()
        }
        async fn send_resp(&mut self, item: ClientResponseStreamItem) {
            self.resp_tx.send(item).unwrap();
        }
    }

    impl Invoke for &MockInvoker {
        type SendStream = MockSendStream;
        type RecvStream = MockRecvStream;

        fn invoke(
            self,
            method: String,
            options: CallOptions,
        ) -> (Self::SendStream, Self::RecvStream) {
            (
                MockSendStream(self.req_tx.clone()),
                MockRecvStream(self.resp_tx.subscribe()),
            )
        }
    }

    struct MockSendStream(mpsc::Sender<(VecDeque<Bytes>, SendOptions)>);
    impl SendStream for MockSendStream {
        async fn send(&mut self, item: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
            self.0
                .send((item.encode().unwrap(), options))
                .await
                .map_err(|_| ())
        }
    }
    struct MockRecvStream(broadcast::Receiver<ClientResponseStreamItem>);
    impl RecvStream for MockRecvStream {
        async fn next(&mut self, msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
            self.0.recv().await.unwrap()
        }
    }

    fn start_retry_streams<I: InvokeFactory>(
        invoker: &I,
        method: String,
        options: CallOptions,
    ) -> (RetrySendStream<I::SendStream>, RetryRecvStream<I>) {
        let invoker = invoker.clone(); // Get an owned InvokeFactory.
        let (send_stream, recv_stream) = invoker.invoke(method.clone(), options.clone());

        let cache = Cache::new();

        (
            RetrySendStream {
                send_stream,
                cache: cache.clone(),
            },
            RetryRecvStream {
                invoker,
                method,
                options,
                recv_stream,
                cache,
                committed: false,
            },
        )
    }

    /// Stores information shared between a retry SendStream/RecvStream pair.
    struct Cache<S> {
        send_stream: Option<S>, // the most recent backing SendStream, if available
        // Set when the stream is committed; SendStream will not wait for a new
        // stream after a send failure when set.
        committed: bool,
        data: Vec<(VecDeque<Bytes>, SendOptions)>, // cached send operations
        // Allows the sender to wait for a new stream.
        notify: Arc<Notify>,
    }

    impl<S> Cache<S> {
        fn new() -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Cache {
                send_stream: None,
                committed: false,
                data: Default::default(),
                notify: Default::default(),
            }))
        }
    }

    struct RetrySendStream<S> {
        send_stream: S, // locally cached send stream
        cache: Arc<Mutex<Cache<S>>>,
    }

    impl<S: SendStream> SendStream for RetrySendStream<S> {
        async fn send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
            loop {
                let res = self.send_stream.send(msg, options.clone()).await;
                let mut cache = self.cache.lock().await;
                if cache.committed {
                    return res;
                }
                if res.is_ok() {
                    // Success; cache this message.
                    cache.data.push((msg.encode().unwrap(), options));
                    return res;
                }
                if cache.send_stream.is_none() {
                    // No new stream and not committed; wait for a new stream.
                    let notify = cache.notify.clone();
                    drop(cache);
                    notify.notified().await;
                    cache = self.cache.lock().await;
                }
                let Some(send_stream) = cache.send_stream.take() else {
                    // We were notified but no new stream was present; return error.
                    return Err(());
                };
                // Retry on the new stream.
                self.send_stream = send_stream;
            }
        }
    }

    pub struct RetryRecvStream<I: InvokeFactory> {
        invoker: I, // the invoker to use to retry calls
        method: String,
        options: CallOptions,
        recv_stream: I::RecvStream, // the most recent attempt's recv_stream
        cache: Arc<Mutex<Cache<I::SendStream>>>,
        // local copy of committed to avoid taking lock held by send operation
        // if we know we have committed.
        committed: bool,
    }

    // Returns true if we can retry a stream based on the response item -- i.e.
    // if it is any error status.  Any other response will commit the RPC.
    fn should_retry(i: &ClientResponseStreamItem) -> bool {
        if let ClientResponseStreamItem::Trailers(t) = &i {
            t.status.code() != StatusCode::Ok
        } else {
            false
        }
    }

    const MAX_ATTEMPTS: usize = 3;

    impl<I: InvokeFactory> RecvStream for RetryRecvStream<I> {
        async fn next(&mut self, msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
            let mut recv_resp = self.recv_stream.next(msg).await;

            if self.committed {
                return recv_resp;
            }
            let mut cache = self.cache.lock().await;
            let mut attempt = 0;
            loop {
                attempt += 1;
                if !should_retry(&recv_resp) || attempt > MAX_ATTEMPTS {
                    self.committed = true;
                    cache.committed = true;
                    cache.data.clear();
                    // Notify the sender in case it is blocked and waiting for a
                    // new stream.
                    cache.notify.notify_waiters();
                    return recv_resp;
                }

                // Retry the whole stream.
                let (mut send_stream, recv_stream) = self
                    .invoker
                    .invoke(self.method.clone(), self.options.clone());
                self.recv_stream = recv_stream;

                // Run the current recv operation in parallel with replaying
                // the stream.
                let recv_fut = self.recv_stream.next(msg);
                pin!(recv_fut);
                let mut recv_state = RecvStreamState::Pending(recv_fut);

                if replay_sends(&mut send_stream, &cache.data, &mut recv_state).await {
                    // Replay completed successfully.  Update the send stream
                    // and release the lock while resolving the recv operation.
                    cache.send_stream = Some(send_stream);
                    cache.notify.notify_waiters();
                    drop(cache);
                    recv_resp = recv_state.resolve().await;
                    cache = self.cache.lock().await;
                } else {
                    // Errors occurred while sending.  Update recv_resp and
                    // re-check while still holding the lock.
                    recv_resp = recv_state.resolve().await;
                }
            }
        }
    }

    async fn replay_sends<S, F>(
        send_stream: &mut S,
        cached_sends: &Vec<(VecDeque<Bytes>, SendOptions)>,
        recv_state: &mut RecvStreamState<F>,
    ) -> bool
    where
        S: SendStream,
        F: Future<Output = ClientResponseStreamItem> + Unpin,
    {
        for (data, options) in cached_sends {
            let send_msg = ByteSendMsg::new(data);
            let send_fut = send_stream.send(&send_msg, options.clone());
            pin!(send_fut);

            // Poll both the recv and send until the send completes or the recv
            // indicates we should retry.
            loop {
                match recv_state.race_with(&mut send_fut).await {
                    Some(res) => {
                        if res.is_err() {
                            return false;
                        }
                        break;
                    }
                    None => {
                        let RecvStreamState::Done(resp) = &recv_state else {
                            unreachable!()
                        };
                        // Abort sending now if we know we need to retry.
                        // Otherwise we will be committing, so we need to finish
                        // replaying the sends.
                        if should_retry(resp) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    // Holds either a Pending() future to a recv call or its Done() result.
    enum RecvStreamState<F> {
        Pending(F),
        Done(ClientResponseStreamItem),
    }

    impl<F: Future<Output = ClientResponseStreamItem> + Unpin> RecvStreamState<F> {
        /// Runs `fut` alongside `self` if it is Pending.  Returns None if `self`
        /// starts Pending and resolves before fut -- `self` then changes to Done
        /// with the result.  Otherwise, returns Some(fut's output) and keeps
        /// `self` in Pending.
        async fn race_with<F2: Future + Unpin>(&mut self, fut: &mut F2) -> Option<F2::Output> {
            match self {
                RecvStreamState::Pending(recv_fut) => {
                    select! {
                        res = recv_fut => {
                            *self = RecvStreamState::Done(res);
                            None
                        }
                        res = fut => { Some(res) }
                    }
                }
                RecvStreamState::Done(_) => Some(fut.await),
            }
        }
        /// Resolves `self`: either returns the already-Done() result of the
        /// recv operation or awaits the future and returns the result.
        async fn resolve(self) -> ClientResponseStreamItem {
            match self {
                RecvStreamState::Pending(fut) => fut.await,
                RecvStreamState::Done(resp) => resp,
            }
        }
    }

    struct ByteRecvMsg {
        data: Option<VecDeque<Bytes>>,
    }
    impl ByteRecvMsg {
        fn new() -> Self {
            Self { data: None }
        }
    }
    impl RecvMessage for ByteRecvMsg {
        fn decode(&mut self, data: &mut VecDeque<Bytes>) -> Result<(), String> {
            self.data = Some(data.clone());
            Ok(())
        }
    }

    struct ByteSendMsg<'a> {
        data: &'a VecDeque<Bytes>,
    }
    impl<'a> ByteSendMsg<'a> {
        fn new(data: &'a VecDeque<Bytes>) -> Self {
            Self { data }
        }
    }
    impl<'a> SendMessage for ByteSendMsg<'a> {
        fn encode(&self) -> Result<VecDeque<Bytes>, String> {
            Ok(self.data.clone())
        }
    }

    #[derive(Clone)]
    struct NopInvoker;

    impl Invoke for &NopInvoker {
        type SendStream = NopStream;
        type RecvStream = NopStream;

        fn invoke(
            self,
            method: String,
            options: CallOptions,
        ) -> (Self::SendStream, Self::RecvStream) {
            (NopStream, NopStream)
        }
    }

    struct NopOnceInvoker;

    impl Invoke for NopOnceInvoker {
        type SendStream = NopStream;
        type RecvStream = NopStream;

        fn invoke(
            self,
            method: String,
            options: CallOptions,
        ) -> (Self::SendStream, Self::RecvStream) {
            (NopStream, NopStream)
        }
    }

    struct NopStream;
    impl SendStream for NopStream {
        async fn send(&mut self, _item: &dyn SendMessage, _options: SendOptions) -> Result<(), ()> {
            Ok(())
        }
    }
    impl RecvStream for NopStream {
        async fn next(
            &mut self,
            _msg: &mut dyn RecvMessage,
        ) -> crate::core::ClientResponseStreamItem {
            ClientResponseStreamItem::StreamClosed
        }
    }
}

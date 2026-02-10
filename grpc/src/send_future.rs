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

/// A helper trait to enforce and explicitly bound a [`Future`] as [`Send`].
///
/// This trait provides a mechanism to work around specific Rust compiler
/// limitations and bugs where the compiler's borrow checker or drop analysis
/// conservatively concludes that an `async` block is `!Send` (not safe to send
/// across threads),
/// even when it logically should be.
///
/// # Problem Context
///
/// As detailed in issues [#64552], [#102211], and [#96865], there are scenarios
/// where:
/// * An `async` function captures a reference to a type that is `!Sync`.
/// * A variable is dropped before an `.await` point, but the compiler's liveness
///   analysis incorrectly believes it is held across the await.
/// * Complex control flow confuses the auto-trait deduction for `Send`.
///
/// These scenarios often result in obscure error messages when trying to spawn
/// the future on an executor (like `tokio::spawn`), claiming the future is not
/// `Send`.
///
/// # The Solution
///
/// The `send()` method acts as an identity function (a no-op at runtime) but
/// performs two critical compile-time tasks:
///
/// 1.  **Explicit Assertion:** It requires `Self` to implement `Send` at the
///     call site. This moves the error message from the deep internals of an
///     executor's spawn function to the specific line where the future is created,
///     making debugging significantly easier.
/// 2.  **Type Erasure / Coercion:** By returning `impl Future<...> + Send`, it
///     creates an opaque type boundary. This can sometimes help the compiler's
///     trait solver "lock in" the `Send` guarantee and disregard phantom lifetime
///     issues that might otherwise propagate.
///
/// # Example
///
/// ```rust,no_run
/// use core::future::Future;
///
/// // Assume this trait is in scope
/// pub trait SendFuture: Future {
///     fn send(self) -> impl Future<Output = Self::Output> + Send
///     where
///         Self: Sized + Send,
///     {
///         self
///     }
/// }
///
/// impl<T: Future> SendFuture for T {}
///
/// async fn complex_logic() {
///     // ... logic that confuses the compiler's Send analysis ...
/// }
///
/// fn spawn_task() {
///     let future = complex_logic();
///
///     // By calling .send(), we explicitly ask the compiler to verify
///     // the Send bound right here.
///     let send_future = future.send();
///
///     // tokio::spawn(send_future);
/// }
/// ```
///
/// [#64552]: https://github.com/rust-lang/rust/issues/64552
/// [#102211]: https://github.com/rust-lang/rust/issues/102211
/// [#96865]: https://github.com/rust-lang/rust/issues/96865
/// [`Future`]: core::future::Future
/// [`Send`]: core::marker::Send
pub trait SendFuture: core::future::Future {
    /// Consumes the future and returns it as an opaque type that is guaranteed
    /// to be [`Send`].
    ///
    /// This is a zero-cost abstraction (it simply returns `self`) used primarily
    /// to help the compiler resolve auto-traits or to produce better error diagnostics.
    fn make_send(self) -> impl core::future::Future<Output = Self::Output> + Send
    where
        Self: Sized + Send,
    {
        self
    }
}

impl<T: core::future::Future> SendFuture for T {}

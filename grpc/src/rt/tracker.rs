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

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::rt::BoxFuture;
use crate::rt::BoxedTaskHandle;
use crate::rt::DnsResolver;
use crate::rt::GrpcEndpoint;
use crate::rt::ResolverOptions;
use crate::rt::Runtime;
use crate::rt::Sleep;
use crate::rt::TcpOptions;
use crate::rt::UnixSocketOptions;

const DEFAULT_TEST_DURATION: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct SharedInnerState {
    tasks: HashMap<u64, Backtrace>,
    next_id: u64,
}

#[derive(Debug)]
struct SharedState {
    inner: Mutex<SharedInnerState>,
    notify: tokio::sync::Notify,
}

struct TaskGuard {
    id: u64,
    state: Arc<SharedState>,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let mut inner = self.state.inner.lock().unwrap();
        inner.tasks.remove(&self.id);
        if inner.tasks.is_empty() {
            self.state.notify.notify_one();
        }
    }
}

/// A `Runtime` wrapper that tracks spawned tasks.
#[derive(Debug)]
pub(crate) struct TrackedRuntime<R> {
    inner: R,
    state: Arc<SharedState>,
}

/// A handle to wait for tasks tracked by `TrackedRuntime`.
pub(crate) struct TaskTracker {
    wait_timeout: Duration,
    state: Arc<SharedState>,
    have_waited: bool,
}

impl<R: Runtime> TrackedRuntime<R> {
    /// Creates a new tracked runtime and its associated tracker.
    ///
    /// Callers must call `wait_for_tasks` on the returned tracker at the end of
    /// the test.
    ///
    /// ```rust
    /// let (tracked_rt, tracker) = TrackedRuntime::new(rt);
    /// tracked_rt.spawn(Box::pin(async {
    ///     // ...
    /// }));
    /// tracker.wait_for_tasks().await;
    /// ```
    pub(crate) fn new(inner: R) -> (Self, TaskTracker) {
        let state = Arc::new(SharedState {
            inner: Mutex::new(SharedInnerState {
                tasks: HashMap::new(),
                next_id: 0,
            }),
            notify: tokio::sync::Notify::new(),
        });
        (
            Self {
                inner,
                state: state.clone(),
            },
            TaskTracker {
                wait_timeout: DEFAULT_TEST_DURATION,
                state,
                have_waited: false,
            },
        )
    }
}

impl TaskTracker {
    /// Waits for all tracked tasks to finish or until timeout.
    ///
    /// It waits for 10 seconds. If the tasks do not finish within this time,
    /// it panics and prints the backtrace of all orphaned tasks.
    ///
    /// Callers MUST call this method before the `TaskTracker` is dropped.
    /// Dropping the `TaskTracker` without calling this method will result in
    /// a panic.
    pub(crate) async fn wait_for_tasks(mut self) {
        self.have_waited = true;
        let notified = self.state.notify.notified();

        if self.state.inner.lock().unwrap().tasks.is_empty() {
            return;
        };

        if tokio::time::timeout(self.wait_timeout, notified)
            .await
            .is_ok()
        {
            return;
        }

        let callsites: Vec<String> = self
            .state
            .inner
            .lock()
            .unwrap()
            .tasks
            .values()
            .map(|bt| format!("{}", bt))
            .collect();

        if callsites.is_empty() {
            // Tasks ended after the timeout expired.
            return;
        }

        panic!(
            "TrackedRuntime: tasks did not end within timeout. Running tasks spawned at:\n{}",
            callsites.join("\n\n---\n\n")
        );
    }
}

impl<R: Runtime> Runtime for TrackedRuntime<R> {
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> BoxedTaskHandle {
        let bt = Backtrace::force_capture();

        let id = {
            let mut inner = self.state.inner.lock().unwrap();
            let id = inner.next_id;
            inner.next_id += 1;
            inner.tasks.insert(id, bt);
            id
        };

        let guard = TaskGuard {
            id,
            state: self.state.clone(),
        };

        let tracked_task = async move {
            // Guard stays alive during await and is dropped when done or
            // cancelled.
            let _guard = guard;
            task.await;
        };

        self.inner.spawn(Box::pin(tracked_task))
    }
    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String> {
        self.inner.get_dns_resolver(opts)
    }

    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>> {
        self.inner.sleep(duration)
    }

    fn tcp_stream(
        &self,
        target: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn GrpcEndpoint>, String>> {
        self.inner.tcp_stream(target, opts)
    }

    fn unix_stream(
        &self,
        path: PathBuf,
        opts: UnixSocketOptions,
    ) -> BoxFuture<Result<Box<dyn GrpcEndpoint>, String>> {
        self.inner.unix_stream(path, opts)
    }
}

impl Drop for TaskTracker {
    fn drop(&mut self) {
        // Check if wait_for_tasks was called and that we are not already
        // panicking to avoid double panics.
        if !self.have_waited && !std::thread::panicking() {
            panic!("TaskTracker was dropped without calling wait_for_tasks!");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::tokio::TokioRuntime;

    #[tokio::test]
    async fn wait_success() {
        let rt = TokioRuntime::default();
        let (tracked_rt, tracker) = TrackedRuntime::new(rt);

        tracked_rt.spawn(Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }));

        tracker.wait_for_tasks().await;
    }

    #[tokio::test]
    #[should_panic(expected = "TrackedRuntime: tasks did not end within timeout")]
    async fn wait_timeout() {
        let rt = TokioRuntime::default();
        let (tracked_rt, mut tracker) = TrackedRuntime::new(rt);
        tracker.wait_timeout = Duration::from_millis(1);

        tracked_rt.spawn(Box::pin(async {
            tokio::time::sleep(DEFAULT_TEST_DURATION).await;
        }));

        tracker.wait_for_tasks().await;
    }

    #[tokio::test]
    #[should_panic(expected = "TaskTracker was dropped without calling wait_for_tasks!")]
    async fn panic_on_drop() {
        let rt = TokioRuntime::default();
        let (_tracked_rt, _tracker) = TrackedRuntime::new(rt);
    }
}

/*
 *
 * Copyright 2025 gRPC authors.
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

pub(crate) mod p2c;
// Ring-hash (gRFC A42) is implemented but not wired into the first-cut LB,
// which selects P2C.
// TODO: wire ring-hash into the LB.
#[allow(dead_code)]
pub(crate) mod ring_hash;

use indexmap::{IndexMap, IndexSet};

use crate::client::endpoint::EndpointAddress;

/// Trait for selecting a channel to handle a request.
///
/// Generic over `S` (the channel type in the ready set) and `Req` (the request).
/// The picker only needs to observe `S`'s load — it doesn't depend on any
/// specific channel state type.
pub(crate) trait ChannelPicker<S, Req> {
    fn pick<'a>(&self, req: &Req, ready: &'a IndexMap<EndpointAddress, S>) -> Option<&'a S>;

    /// Notify the picker that the cluster's member set changed.
    ///
    /// `members` is the full healthy-EDS membership, independent of connection
    /// or ejection state. Stateful pickers (e.g. ring-hash) rebuild their
    /// internal structure here so the ring stays stable across connection
    /// flaps and ejections (which do not change membership). Default: no-op
    /// (e.g. [`p2c::P2cPicker`], which is stateless).
    fn on_members_changed(&self, _members: &IndexSet<EndpointAddress>) {}
}

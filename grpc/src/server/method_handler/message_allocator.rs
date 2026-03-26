use crate::server::message::{AsMut, AsView};
use std::marker::PhantomData;

/// A trait for allocating messages.
pub trait RpcMessageAllocator<Req, Resp>: Send + Sync
where
    Req: AsMut,
    Resp: AsMut,
{
    /// The type of the message holder returned by this allocator.
    type Holder: RpcMessageHolder<Req, Resp>;

    /// Allocates a new message holder.
    fn allocate(&self) -> Self::Holder;
}

/// A trait for holding a request message.
pub trait RpcRequestHolder<Req>: Send
where
    Req: AsMut,
{
    /// Returns a mutable reference to the request message.
    fn get_request_mut(&mut self) -> <Req as AsMut>::Mut<'_>;
}

/// A trait for holding a response message.
pub trait RpcResponseHolder<Resp>: Send
where
    Resp: AsMut,
{
    /// Returns a mutable reference to the response message.
    fn get_response_mut(&mut self) -> <Resp as AsMut>::Mut<'_>;
}

/// A trait for holding messages and providing access to them.
pub trait RpcMessageHolder<Req, Resp>:
    RpcRequestHolder<Req> + RpcResponseHolder<Resp> + Send
where
    Req: AsMut,
    Resp: AsMut,
{
    /// Returns mutable references to both request and response messages.
    fn get_muts(&mut self) -> (<Req as AsMut>::Mut<'_>, <Resp as AsMut>::Mut<'_>);

    /// Returns a view of the request message and a mutable reference to the response message.
    ///
    /// This method is provided to avoid lifetime issues that can occur when calling
    /// `get_muts()` and then calling `as_view()` on the request mutable reference.
    fn get_request_view_and_response_mut(
        &mut self,
    ) -> (<Req as AsView>::View<'_>, <Resp as AsMut>::Mut<'_>)
    where
        Req: AsView;
}

/// A message allocator that allocates messages on the heap.
pub struct HeapMessageAllocator<Req, Resp> {
    _pd: PhantomData<fn(Req, Resp)>,
}

impl<Req, Resp> Default for HeapMessageAllocator<Req, Resp> {
    fn default() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<Req, Resp> HeapMessageAllocator<Req, Resp> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<Req, Resp> RpcMessageAllocator<Req, Resp> for HeapMessageAllocator<Req, Resp>
where
    Req: Default + AsMut + AsView + Send,
    Resp: Default + AsMut + Send,
{
    type Holder = HeapMessageHolder<Req, Resp>;

    fn allocate(&self) -> Self::Holder {
        HeapMessageHolder {
            req: Req::default(),
            resp: Resp::default(),
        }
    }
}

/// A message holder that holds messages on the heap.
pub struct HeapMessageHolder<Req, Resp> {
    req: Req,
    resp: Resp,
}

impl<Req, Resp> RpcRequestHolder<Req> for HeapMessageHolder<Req, Resp>
where
    Req: AsMut + Send,
    Resp: Send,
{
    fn get_request_mut(&mut self) -> <Req as AsMut>::Mut<'_> {
        self.req.as_mut()
    }
}

impl<Req, Resp> RpcResponseHolder<Resp> for HeapMessageHolder<Req, Resp>
where
    Resp: AsMut + Send,
    Req: Send,
{
    fn get_response_mut(&mut self) -> <Resp as AsMut>::Mut<'_> {
        self.resp.as_mut()
    }
}

impl<Req, Resp> RpcMessageHolder<Req, Resp> for HeapMessageHolder<Req, Resp>
where
    Req: AsMut + AsView + Send,
    Resp: AsMut + Send,
{
    fn get_muts(&mut self) -> (<Req as AsMut>::Mut<'_>, <Resp as AsMut>::Mut<'_>) {
        (self.req.as_mut(), self.resp.as_mut())
    }

    fn get_request_view_and_response_mut(
        &mut self,
    ) -> (<Req as AsView>::View<'_>, <Resp as AsMut>::Mut<'_>)
    where
        Req: AsView,
    {
        (self.req.as_view(), self.resp.as_mut())
    }
}

/// A request holder that holds a request message on the heap.
pub struct HeapRequestHolder<Req> {
    req: Req,
}

impl<Req> RpcRequestHolder<Req> for HeapRequestHolder<Req>
where
    Req: AsMut + Send,
{
    fn get_request_mut(&mut self) -> <Req as AsMut>::Mut<'_> {
        self.req.as_mut()
    }
}

impl<Req> HeapRequestHolder<Req> {
    pub fn new(req: Req) -> Self {
        Self { req }
    }
}

/// A response holder that holds a response message on the heap.
pub struct HeapResponseHolder<Resp> {
    resp: Resp,
}

impl<Resp> HeapResponseHolder<Resp> {
    pub fn new(resp: Resp) -> Self {
        Self { resp }
    }
}

impl<Resp> RpcResponseHolder<Resp> for HeapResponseHolder<Resp>
where
    Resp: AsMut + Send,
{
    fn get_response_mut(&mut self) -> <Resp as AsMut>::Mut<'_> {
        self.resp.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use protobuf_well_known_types::Timestamp;

    #[test]
    fn test_heap_message_allocator_allocate() {
        let allocator = HeapMessageAllocator::<Timestamp, Timestamp>::new();
        let holder = allocator.allocate();
        assert_eq!(holder.req.seconds(), 0);
        assert_eq!(holder.resp.seconds(), 0);
    }

    #[test]
    fn test_heap_message_holder_accessors() {
        let mut holder = HeapMessageHolder {
            req: Timestamp::new(),
            resp: Timestamp::new(),
        };
        holder.req.set_seconds(10);
        holder.resp.set_seconds(20);

        // Test get_request_mut (via RpcRequestHolder trait)
        {
            let mut req_mut = holder.get_request_mut();
            req_mut.set_seconds(11);
        }
        assert_eq!(holder.req.seconds(), 11);

        // Test get_response_mut (via RpcResponseHolder trait)
        {
            let mut resp_mut = holder.get_response_mut();
            resp_mut.set_seconds(21);
        }
        assert_eq!(holder.resp.seconds(), 21);

        // Test get_muts
        {
            let (mut req_mut, mut resp_mut) = holder.get_muts();
            req_mut.set_seconds(31);
            resp_mut.set_seconds(41);
        }
        assert_eq!(holder.req.seconds(), 31);
        assert_eq!(holder.resp.seconds(), 41);

        // Test get_request_view_and_response_mut
        {
            let (req_view, _) = holder.get_request_view_and_response_mut();
            assert_eq!(req_view.seconds(), 31);
        }
    }
}

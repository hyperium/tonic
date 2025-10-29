use super::{AsMut, AsView};

impl<T> AsView for T
where
    T: protobuf::AsView + Sync,
    // Require `Copy` to emulate "all shared references are copyable"
    for<'msg> protobuf::View<'msg, <T as protobuf::AsView>::Proxied>: Copy,
{
    type View<'msg>
        = protobuf::View<'msg, <T as protobuf::AsView>::Proxied>
    where
        Self: 'msg;

    fn as_view(&self) -> Self::View<'_> {
        self.as_view()
    }
}

impl<T> AsMut for T
where
    T: protobuf::AsMut + Send + 'static,
    for<'msg> protobuf::Mut<'msg, <T as protobuf::AsMut>::MutProxied>: Send + AsView,
{
    type Mut<'a> = protobuf::Mut<'a, <T as protobuf::AsMut>::MutProxied>;

    fn as_mut(&mut self) -> Self::Mut<'_> {
        protobuf::AsMut::as_mut(self)
    }

    fn reborrow_view<'a, 'b>(view: &'b mut Self::Mut<'a>) -> Self::Mut<'b>
    where
        'a: 'b,
    {
        protobuf::AsMut::as_mut(view)
    }
}

#[cfg(test)]
mod tests {
    use crate::server::message::{AsMut, AsView};

    #[test]
    fn test_protobuf_view() {
        use protobuf_well_known_types::Timestamp;
        let mut ts = Timestamp::new();
        ts.set_seconds(1234567890);
        let view = AsView::as_view(&ts);
        assert_eq!(view.seconds(), 1234567890);
    }

    #[test]
    fn test_protobuf_as_mut() {
        use protobuf_well_known_types::Timestamp;

        let mut msg = Timestamp::new();
        msg.set_seconds(123);
        msg.set_nanos(456);

        // 1. Test as_mut()
        let mut view = AsMut::as_mut(&mut msg);
        assert_eq!(view.seconds(), 123);
        view.set_seconds(789);

        // 2. Test reborrow_view()
        {
            let mut reborrowed = <Timestamp as AsMut>::reborrow_view(&mut view);
            assert_eq!(reborrowed.seconds(), 789);
            reborrowed.set_nanos(999);
        }

        // 3. Verify view is still usable (reborrow, not move)
        assert_eq!(view.nanos(), 999);
        view.set_seconds(111);

        // Verify changes persisted
        assert_eq!(msg.seconds(), 111);
        assert_eq!(msg.nanos(), 999);
    }
}

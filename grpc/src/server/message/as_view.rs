pub trait AsView {
    /// A GAT for the view type.
    type View<'msg>: Send + Copy
    where
        Self: 'msg;

    /// Creates a view of the object.
    fn as_view(&self) -> Self::View<'_>;
}

#[cfg(test)]
mod tests {
    use super::AsView;

    #[test]
    fn test_non_protobuf_view() {
        #[derive(Debug, PartialEq)]
        struct TestMessage {
            id: i32,
        }

        impl AsView for TestMessage {
            type View<'msg>
                = &'msg TestMessage
            where
                Self: 'msg;
            fn as_view(&self) -> Self::View<'_> {
                self
            }
        }

        let msg = TestMessage { id: 42 };
        let view = AsView::as_view(&msg);
        assert_eq!(view.id, 42);
        assert!(std::ptr::eq(view, &msg));
    }
}

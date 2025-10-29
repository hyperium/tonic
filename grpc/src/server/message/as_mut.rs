use crate::server::message::AsView;

pub trait AsMut {
    /// We let this be whatever the library uses (e.g. protobuf::Mut).
    type Mut<'a>: Send + AsView + 'a;

    fn as_mut(&mut self) -> Self::Mut<'_>;

    /// Implement `reborrow` to make it feel it feel like a mut reference.
    /// Since we can't attach .reborrow() to the alias 'Self::Mut',
    /// we define the logic here as a static utility function.
    #[doc(hidden)]
    fn reborrow_view<'a, 'b>(view: &'b mut Self::Mut<'a>) -> Self::Mut<'b>
    where
        'a: 'b;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_as_mut() {
        struct TestMsg {
            val: i32,
        }

        struct TestMsgMut<'a> {
            val: &'a mut i32,
        }

        impl AsView for TestMsgMut<'_> {
            type View<'msg>
                = i32
            where
                Self: 'msg;
            fn as_view(&self) -> Self::View<'_> {
                *self.val
            }
        }

        impl AsMut for TestMsg {
            type Mut<'a> = TestMsgMut<'a>;

            fn as_mut(&mut self) -> Self::Mut<'_> {
                TestMsgMut { val: &mut self.val }
            }

            fn reborrow_view<'a, 'b>(view: &'b mut Self::Mut<'a>) -> Self::Mut<'b>
            where
                'a: 'b,
            {
                TestMsgMut { val: view.val }
            }
        }

        let mut msg = TestMsg { val: 10 };

        // 1. Test as_mut()
        let mut view = msg.as_mut();
        assert_eq!(*view.val, 10);
        *view.val = 20;

        // 2. Test reborrow_view()
        {
            let reborrowed = <TestMsg as AsMut>::reborrow_view(&mut view);
            assert_eq!(*reborrowed.val, 20);
            *reborrowed.val = 30;
        }

        // 3. Verify view is still usable (reborrow, not move)
        assert_eq!(*view.val, 30);
        *view.val = 40;

        // Verify changes persisted
        assert_eq!(msg.val, 40);
    }
}

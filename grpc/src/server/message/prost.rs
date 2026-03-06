use super::{AsMut, AsView};

impl<T> AsView for T
where
    T: prost::Message + Sync + Send,
{
    type View<'msg>
        = &'msg T
    where
        Self: 'msg;

    fn as_view(&self) -> Self::View<'_> {
        self
    }
}

impl<T> AsMut for T
where
    T: prost::Message + Send + 'static,
{
    type Mut<'a> = &'a mut T;

    fn as_mut(&mut self) -> Self::Mut<'_> {
        self
    }

    fn reborrow_view<'a, 'b>(view: &'b mut Self::Mut<'a>) -> Self::Mut<'b>
    where
        'a: 'b,
    {
        &mut **view
    }
}

#[cfg(test)]
mod tests {
    use crate::server::message::{AsMut, AsView};
    use prost::Message;

    // A simple prost message for testing
    #[derive(Clone, PartialEq, Message)]
    struct ValidateRequest {
        #[prost(string, tag = "1")]
        pub service_name: String,
    }

    #[test]
    fn test_prost_view() {
        let msg = ValidateRequest {
            service_name: "test_service".to_string(),
        };
        let view = AsView::as_view(&msg);
        assert_eq!(view.service_name, "test_service");
        // For prost, the view is just a reference
        assert_eq!(view as *const _, &msg as *const _);
    }

    #[test]
    fn test_prost_as_mut() {
        let mut msg = ValidateRequest {
            service_name: "test_service".to_string(),
        };

        // 1. Test as_mut()
        let mut view = AsMut::as_mut(&mut msg);
        assert_eq!(view.service_name, "test_service");
        view.service_name = "updated_service".to_string();

        // 2. Test reborrow_view()
        {
            let reborrowed = <ValidateRequest as AsMut>::reborrow_view(&mut view);
            assert_eq!(reborrowed.service_name, "updated_service");
            reborrowed.service_name = "reborrowed_service".to_string();
        }

        // 3. Verify view is still usable
        assert_eq!(view.service_name, "reborrowed_service");
        view.service_name = "final_service".to_string();

        // Verify changes persisted
        assert_eq!(msg.service_name, "final_service");
    }
}

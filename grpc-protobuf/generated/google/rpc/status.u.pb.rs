const _: () = ::protobuf::__internal::assert_compatible_gencode_version("0.36.2-release");
// This variable must not be referenced except by protobuf generated
// code.
pub(crate) static mut google__rpc__Status_msg_init: ::protobuf::__internal::runtime::MiniTableInitPtr =
    ::protobuf::__internal::runtime::MiniTableInitPtr(::protobuf::__internal::runtime::MiniTablePtr::dangling());
#[allow(non_camel_case_types)]
pub struct Status {
  inner: ::protobuf::__internal::runtime::OwnedMessageInner<Status>
}

impl ::protobuf::Message for Status {
  type MessageView<'msg> = StatusView<'msg>;
  type MessageMut<'msg> = StatusMut<'msg>;
}

impl ::std::default::Default for Status {
  fn default() -> Self {
    Self::new()
  }
}

impl ::std::fmt::Debug for Status {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

// SAFETY:
// - `Status` is `Sync` because it does not implement interior mutability.
//    Neither does `StatusMut`.
unsafe impl ::std::marker::Sync for Status {}

// SAFETY:
// - `Status` is `Send` because it uniquely owns its arena and does
//   not use thread-local data.
unsafe impl ::std::marker::Send for Status {}

impl ::protobuf::Proxied for Status {
  type View<'msg> = StatusView<'msg>;
}

impl ::protobuf::__internal::SealedInternal for Status {}

impl ::protobuf::MutProxied for Status {
  type Mut<'msg> = StatusMut<'msg>;
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub struct StatusView<'msg> {
  inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, Status>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for StatusView<'msg> {}

impl<'msg> ::protobuf::MessageView<'msg> for StatusView<'msg> {
  type Message = Status;
}

impl ::std::fmt::Debug for StatusView<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl ::std::default::Default for StatusView<'_> {
  fn default() -> StatusView<'static> {
    ::protobuf::__internal::runtime::MessageViewInner::default().into()
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageViewInner<'msg, Status>> for StatusView<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, Status>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> StatusView<'msg> {

  pub fn to_owned(&self) -> Status {
    ::protobuf::IntoProxied::into_proxied(*self, ::protobuf::__internal::Private)
  }

  // code: optional int32
  pub fn code(self) -> i32 {
    unsafe {
      // TODO: b/361751487: This .into() and .try_into() is only
      // here for the enum<->i32 case, we should avoid it for
      // other primitives where the types naturally match
      // perfectly (and do an unchecked conversion for
      // i32->enum types, since even for closed enums we trust
      // upb to only return one of the named values).
      self.inner.ptr().get_i32_at_index(
        0, (0i32).into()
      ).try_into().unwrap()
    }
  }

  // message: optional string
  pub fn message(self) -> ::protobuf::View<'msg, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        1, (b"").into()
      )
    };
    ::protobuf::ProtoStr::from_utf8_unchecked(unsafe { str_view.as_ref() })
  }

  // details: repeated message google.protobuf.Any
  pub fn details(self) -> ::protobuf::RepeatedView<'msg, ::protobuf_well_known_types::google_protobuf_any_proto::Any> {
    unsafe {
      self.inner.ptr().get_array_at_index(
        2
      )
    }.map_or_else(
        ::protobuf::__internal::runtime::empty_array::<::protobuf_well_known_types::google_protobuf_any_proto::Any>,
        |raw| unsafe {
          ::protobuf::RepeatedView::from_raw(::protobuf::__internal::Private, raw)
        }
      )
  }

}

// SAFETY:
// - `StatusView` is `Sync` because it does not support mutation.
unsafe impl ::std::marker::Sync for StatusView<'_> {}

// SAFETY:
// - `StatusView` is `Send` because while its alive a `StatusMut` cannot.
// - `StatusView` does not use thread-local data.
unsafe impl ::std::marker::Send for StatusView<'_> {}

impl<'msg> ::protobuf::AsView for StatusView<'msg> {
  type Proxied = Status;
  fn as_view(&self) -> ::protobuf::View<'msg, Status> {
    *self
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for StatusView<'msg> {
  fn into_view<'shorter>(self) -> StatusView<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

impl<'msg> ::protobuf::IntoProxied<Status> for StatusView<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> Status {
    let mut dst = Status::new();
    assert!(unsafe {
      dst.inner.ptr_mut().deep_copy(self.inner.ptr(), dst.inner.arena())
    });
    dst
  }
}

impl<'msg> ::protobuf::IntoProxied<Status> for StatusMut<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> Status {
    ::protobuf::IntoProxied::into_proxied(::protobuf::IntoView::into_view(self), _private)
  }
}

impl ::protobuf::__internal::EntityType for Status {
    type Tag = ::protobuf::__internal::entity_tag::MessageTag;
}

impl<'msg> ::protobuf::__internal::EntityType for StatusView<'msg> {
    type Tag = ::protobuf::__internal::entity_tag::ViewProxyTag;
}

impl<'msg> ::protobuf::__internal::EntityType for StatusMut<'msg> {
    type Tag = ::protobuf::__internal::entity_tag::MutProxyTag;
}

#[allow(dead_code)]
#[allow(non_camel_case_types)]
pub struct StatusMut<'msg> {
  inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, Status>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for StatusMut<'msg> {}

impl<'msg> ::protobuf::MessageMut<'msg> for StatusMut<'msg> {
  type Message = Status;
}

impl ::std::fmt::Debug for StatusMut<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageMutInner<'msg, Status>> for StatusMut<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, Status>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> StatusMut<'msg> {

  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private)
    -> ::protobuf::__internal::runtime::MessageMutInner<'msg, Status> {
    self.inner.reborrow()
  }

  pub fn to_owned(&self) -> Status {
    ::protobuf::AsView::as_view(self).to_owned()
  }

  // code: optional int32
  pub fn code(&self) -> i32 {
    unsafe {
      // TODO: b/361751487: This .into() and .try_into() is only
      // here for the enum<->i32 case, we should avoid it for
      // other primitives where the types naturally match
      // perfectly (and do an unchecked conversion for
      // i32->enum types, since even for closed enums we trust
      // upb to only return one of the named values).
      self.inner.ptr().get_i32_at_index(
        0, (0i32).into()
      ).try_into().unwrap()
    }
  }
  pub fn set_code(&mut self, val: i32) {
    unsafe {
      // TODO: b/361751487: This .into() is only here
      // here for the enum<->i32 case, we should avoid it for
      // other primitives where the types naturally match
      //perfectly.
      self.inner.ptr_mut().set_base_field_i32_at_index(
        0, val.into()
      )
    }
  }

  // message: optional string
  pub fn message(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        1, (b"").into()
      )
    };
    ::protobuf::ProtoStr::from_utf8_unchecked(unsafe { str_view.as_ref() })
  }
  pub fn set_message(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        1,
        val);
    }
  }

  // details: repeated message google.protobuf.Any
  pub fn details(&self) -> ::protobuf::RepeatedView<'_, ::protobuf_well_known_types::google_protobuf_any_proto::Any> {
    unsafe {
      self.inner.ptr().get_array_at_index(
        2
      )
    }.map_or_else(
        ::protobuf::__internal::runtime::empty_array::<::protobuf_well_known_types::google_protobuf_any_proto::Any>,
        |raw| unsafe {
          ::protobuf::RepeatedView::from_raw(::protobuf::__internal::Private, raw)
        }
      )
  }
  pub fn details_mut(&mut self) -> ::protobuf::RepeatedMut<'_, ::protobuf_well_known_types::google_protobuf_any_proto::Any> {
    unsafe {
      let raw_array = self.inner.ptr_mut().get_or_create_mutable_array_at_index(
        2,
        self.inner.arena()
      ).expect("alloc should not fail");
      ::protobuf::RepeatedMut::from_inner(
        ::protobuf::__internal::Private,
        ::protobuf::__internal::runtime::InnerRepeatedMut::new(
          raw_array, self.inner.arena(),
        ),
      )
    }
  }
  pub fn set_details(&mut self, src: impl ::protobuf::IntoProxied<::protobuf::Repeated<::protobuf_well_known_types::google_protobuf_any_proto::Any>>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_repeated_field(
        ::protobuf::AsMut::as_mut(self).inner,
        2,
        src);
    }
  }

}

// SAFETY:
// - `StatusMut` does not perform any shared mutation.
unsafe impl ::std::marker::Send for StatusMut<'_> {}

// SAFETY:
// - `StatusMut` does not perform any shared mutation.
unsafe impl ::std::marker::Sync for StatusMut<'_> {}

impl<'msg> ::protobuf::AsView for StatusMut<'msg> {
  type Proxied = Status;
  fn as_view(&self) -> ::protobuf::View<'_, Status> {
    self.inner.as_view().into()
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for StatusMut<'msg> {
  fn into_view<'shorter>(self) -> ::protobuf::View<'shorter, Status>
  where
      'msg: 'shorter {
    self.inner.as_view().into()
  }
}

impl<'msg> ::protobuf::AsMut for StatusMut<'msg> {
  type MutProxied = Status;
  fn as_mut(&mut self) -> StatusMut<'msg> {
    self.inner.reborrow().into()
  }
}

impl<'msg> ::protobuf::IntoMut<'msg> for StatusMut<'msg> {
  fn into_mut<'shorter>(self) -> StatusMut<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

#[allow(dead_code)]
impl Status {
  pub fn new() -> Self {
    Self { inner: ::protobuf::__internal::runtime::OwnedMessageInner::<Self>::new() }
  }


  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessageMutInner<'_, Status> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner)
  }

  pub fn as_view(&self) -> StatusView<'_> {
    ::protobuf::__internal::runtime::MessageViewInner::view_of_owned(&self.inner).into()
  }

  pub fn as_mut(&mut self) -> StatusMut<'_> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner).into()
  }

  // code: optional int32
  pub fn code(&self) -> i32 {
    unsafe {
      // TODO: b/361751487: This .into() and .try_into() is only
      // here for the enum<->i32 case, we should avoid it for
      // other primitives where the types naturally match
      // perfectly (and do an unchecked conversion for
      // i32->enum types, since even for closed enums we trust
      // upb to only return one of the named values).
      self.inner.ptr().get_i32_at_index(
        0, (0i32).into()
      ).try_into().unwrap()
    }
  }
  pub fn set_code(&mut self, val: i32) {
    unsafe {
      // TODO: b/361751487: This .into() is only here
      // here for the enum<->i32 case, we should avoid it for
      // other primitives where the types naturally match
      //perfectly.
      self.inner.ptr_mut().set_base_field_i32_at_index(
        0, val.into()
      )
    }
  }

  // message: optional string
  pub fn message(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        1, (b"").into()
      )
    };
    ::protobuf::ProtoStr::from_utf8_unchecked(unsafe { str_view.as_ref() })
  }
  pub fn set_message(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        1,
        val);
    }
  }

  // details: repeated message google.protobuf.Any
  pub fn details(&self) -> ::protobuf::RepeatedView<'_, ::protobuf_well_known_types::google_protobuf_any_proto::Any> {
    unsafe {
      self.inner.ptr().get_array_at_index(
        2
      )
    }.map_or_else(
        ::protobuf::__internal::runtime::empty_array::<::protobuf_well_known_types::google_protobuf_any_proto::Any>,
        |raw| unsafe {
          ::protobuf::RepeatedView::from_raw(::protobuf::__internal::Private, raw)
        }
      )
  }
  pub fn details_mut(&mut self) -> ::protobuf::RepeatedMut<'_, ::protobuf_well_known_types::google_protobuf_any_proto::Any> {
    unsafe {
      let raw_array = self.inner.ptr_mut().get_or_create_mutable_array_at_index(
        2,
        self.inner.arena()
      ).expect("alloc should not fail");
      ::protobuf::RepeatedMut::from_inner(
        ::protobuf::__internal::Private,
        ::protobuf::__internal::runtime::InnerRepeatedMut::new(
          raw_array, self.inner.arena(),
        ),
      )
    }
  }
  pub fn set_details(&mut self, src: impl ::protobuf::IntoProxied<::protobuf::Repeated<::protobuf_well_known_types::google_protobuf_any_proto::Any>>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_repeated_field(
        ::protobuf::AsMut::as_mut(self).inner,
        2,
        src);
    }
  }

}  // impl Status

impl ::std::ops::Drop for Status {
  #[inline]
  fn drop(&mut self) {
  }
}

impl ::std::clone::Clone for Status {
  fn clone(&self) -> Self {
    self.as_view().to_owned()
  }
}

impl ::protobuf::AsView for Status {
  type Proxied = Self;
  fn as_view(&self) -> StatusView<'_> {
    self.as_view()
  }
}

impl ::protobuf::AsMut for Status {
  type MutProxied = Self;
  fn as_mut(&mut self) -> StatusMut<'_> {
    self.as_mut()
  }
}

unsafe impl ::protobuf::__internal::runtime::AssociatedMiniTable for Status {
  fn mini_table() -> ::protobuf::__internal::runtime::MiniTablePtr {
    static ONCE_LOCK: ::std::sync::OnceLock<::protobuf::__internal::runtime::MiniTableInitPtr> =
        ::std::sync::OnceLock::new();
    unsafe {
      ONCE_LOCK.get_or_init(|| {
        super::google_rpc_status_proto::google__rpc__Status_msg_init.0 =
            ::protobuf::__internal::runtime::build_mini_table("$(P1XG");
        ::protobuf::__internal::runtime::link_mini_table(
            super::google_rpc_status_proto::google__rpc__Status_msg_init.0, &[<::protobuf_well_known_types::google_protobuf_any_proto::Any as ::protobuf::__internal::runtime::AssociatedMiniTable>::mini_table(),
            ], &[]);
        ::protobuf::__internal::runtime::MiniTableInitPtr(super::google_rpc_status_proto::google__rpc__Status_msg_init.0)
      }).0
    }
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetArena for Status {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for Status {
  type Msg = Status;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<Status> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for Status {
  type Msg = Status;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<Status> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for StatusMut<'_> {
  type Msg = Status;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<Status> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for StatusMut<'_> {
  type Msg = Status;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<Status> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for StatusView<'_> {
  type Msg = Status;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<Status> {
    self.inner.ptr()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetArena for StatusMut<'_> {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}




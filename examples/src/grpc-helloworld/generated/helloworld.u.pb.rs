const _: () = ::protobuf::__internal::assert_compatible_gencode_version("4.34.0-release");
// This variable must not be referenced except by protobuf generated
// code.
pub(crate) static mut helloworld__HelloRequest_msg_init: ::protobuf::__internal::runtime::MiniTableInitPtr =
    ::protobuf::__internal::runtime::MiniTableInitPtr(::protobuf::__internal::runtime::MiniTablePtr::dangling());
#[allow(non_camel_case_types)]
pub struct HelloRequest {
  inner: ::protobuf::__internal::runtime::OwnedMessageInner<HelloRequest>
}

impl ::protobuf::Message for HelloRequest {}

impl ::std::default::Default for HelloRequest {
  fn default() -> Self {
    Self::new()
  }
}

impl ::std::fmt::Debug for HelloRequest {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

// SAFETY:
// - `HelloRequest` is `Sync` because it does not implement interior mutability.
//    Neither does `HelloRequestMut`.
unsafe impl Sync for HelloRequest {}

// SAFETY:
// - `HelloRequest` is `Send` because it uniquely owns its arena and does
//   not use thread-local data.
unsafe impl Send for HelloRequest {}

impl ::protobuf::Proxied for HelloRequest {
  type View<'msg> = HelloRequestView<'msg>;
}

impl ::protobuf::__internal::SealedInternal for HelloRequest {}

impl ::protobuf::MutProxied for HelloRequest {
  type Mut<'msg> = HelloRequestMut<'msg>;
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub struct HelloRequestView<'msg> {
  inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, HelloRequest>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for HelloRequestView<'msg> {}

impl<'msg> ::protobuf::MessageView<'msg> for HelloRequestView<'msg> {
  type Message = HelloRequest;
}

impl ::std::fmt::Debug for HelloRequestView<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl ::std::default::Default for HelloRequestView<'_> {
  fn default() -> HelloRequestView<'static> {
    ::protobuf::__internal::runtime::MessageViewInner::default().into()
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageViewInner<'msg, HelloRequest>> for HelloRequestView<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, HelloRequest>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> HelloRequestView<'msg> {

  pub fn to_owned(&self) -> HelloRequest {
    ::protobuf::IntoProxied::into_proxied(*self, ::protobuf::__internal::Private)
  }

  // name: optional string
  pub fn name(self) -> ::protobuf::View<'msg, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }

}

// SAFETY:
// - `HelloRequestView` is `Sync` because it does not support mutation.
unsafe impl Sync for HelloRequestView<'_> {}

// SAFETY:
// - `HelloRequestView` is `Send` because while its alive a `HelloRequestMut` cannot.
// - `HelloRequestView` does not use thread-local data.
unsafe impl Send for HelloRequestView<'_> {}

impl<'msg> ::protobuf::AsView for HelloRequestView<'msg> {
  type Proxied = HelloRequest;
  fn as_view(&self) -> ::protobuf::View<'msg, HelloRequest> {
    *self
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for HelloRequestView<'msg> {
  fn into_view<'shorter>(self) -> HelloRequestView<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

impl<'msg> ::protobuf::IntoProxied<HelloRequest> for HelloRequestView<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> HelloRequest {
    let mut dst = HelloRequest::new();
    assert!(unsafe {
      dst.inner.ptr_mut().deep_copy(self.inner.ptr(), dst.inner.arena())
    });
    dst
  }
}

impl<'msg> ::protobuf::IntoProxied<HelloRequest> for HelloRequestMut<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> HelloRequest {
    ::protobuf::IntoProxied::into_proxied(::protobuf::IntoView::into_view(self), _private)
  }
}

impl ::protobuf::__internal::runtime::EntityType for HelloRequest {
    type Tag = ::protobuf::__internal::runtime::MessageTag;
}

impl<'msg> ::protobuf::__internal::runtime::EntityType for HelloRequestView<'msg> {
    type Tag = ::protobuf::__internal::runtime::ViewProxyTag;
}

impl<'msg> ::protobuf::__internal::runtime::EntityType for HelloRequestMut<'msg> {
    type Tag = ::protobuf::__internal::runtime::MutProxyTag;
}

#[allow(dead_code)]
#[allow(non_camel_case_types)]
pub struct HelloRequestMut<'msg> {
  inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloRequest>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for HelloRequestMut<'msg> {}

impl<'msg> ::protobuf::MessageMut<'msg> for HelloRequestMut<'msg> {
  type Message = HelloRequest;
}

impl ::std::fmt::Debug for HelloRequestMut<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageMutInner<'msg, HelloRequest>> for HelloRequestMut<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloRequest>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> HelloRequestMut<'msg> {

  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private)
    -> ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloRequest> {
    self.inner
  }

  pub fn to_owned(&self) -> HelloRequest {
    ::protobuf::AsView::as_view(self).to_owned()
  }

  // name: optional string
  pub fn name(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }
  pub fn set_name(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        0,
        val);
    }
  }

}

// SAFETY:
// - `HelloRequestMut` does not perform any shared mutation.
unsafe impl Send for HelloRequestMut<'_> {}

// SAFETY:
// - `HelloRequestMut` does not perform any shared mutation.
unsafe impl Sync for HelloRequestMut<'_> {}

impl<'msg> ::protobuf::AsView for HelloRequestMut<'msg> {
  type Proxied = HelloRequest;
  fn as_view(&self) -> ::protobuf::View<'_, HelloRequest> {
    HelloRequestView {
      inner: ::protobuf::__internal::runtime::MessageViewInner::view_of_mut(self.inner)
    }
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for HelloRequestMut<'msg> {
  fn into_view<'shorter>(self) -> ::protobuf::View<'shorter, HelloRequest>
  where
      'msg: 'shorter {
    HelloRequestView {
      inner: ::protobuf::__internal::runtime::MessageViewInner::view_of_mut(self.inner)
    }
  }
}

impl<'msg> ::protobuf::AsMut for HelloRequestMut<'msg> {
  type MutProxied = HelloRequest;
  fn as_mut(&mut self) -> HelloRequestMut<'msg> {
    HelloRequestMut { inner: self.inner }
  }
}

impl<'msg> ::protobuf::IntoMut<'msg> for HelloRequestMut<'msg> {
  fn into_mut<'shorter>(self) -> HelloRequestMut<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

#[allow(dead_code)]
impl HelloRequest {
  pub fn new() -> Self {
    Self { inner: ::protobuf::__internal::runtime::OwnedMessageInner::<Self>::new() }
  }


  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessageMutInner<'_, HelloRequest> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner)
  }

  pub fn as_view(&self) -> HelloRequestView<'_> {
    ::protobuf::__internal::runtime::MessageViewInner::view_of_owned(&self.inner).into()
  }

  pub fn as_mut(&mut self) -> HelloRequestMut<'_> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner).into()
  }

  // name: optional string
  pub fn name(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }
  pub fn set_name(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        0,
        val);
    }
  }

}  // impl HelloRequest

impl ::std::ops::Drop for HelloRequest {
  #[inline]
  fn drop(&mut self) {
  }
}

impl ::std::clone::Clone for HelloRequest {
  fn clone(&self) -> Self {
    self.as_view().to_owned()
  }
}

impl ::protobuf::AsView for HelloRequest {
  type Proxied = Self;
  fn as_view(&self) -> HelloRequestView<'_> {
    self.as_view()
  }
}

impl ::protobuf::AsMut for HelloRequest {
  type MutProxied = Self;
  fn as_mut(&mut self) -> HelloRequestMut<'_> {
    self.as_mut()
  }
}

unsafe impl ::protobuf::__internal::runtime::AssociatedMiniTable for HelloRequest {
  fn mini_table() -> ::protobuf::__internal::runtime::MiniTablePtr {
    static ONCE_LOCK: ::std::sync::OnceLock<::protobuf::__internal::runtime::MiniTableInitPtr> =
        ::std::sync::OnceLock::new();
    unsafe {
      ONCE_LOCK.get_or_init(|| {
        super::helloworld__HelloRequest_msg_init.0 =
            ::protobuf::__internal::runtime::build_mini_table("$M1P");
        ::protobuf::__internal::runtime::link_mini_table(
            super::helloworld__HelloRequest_msg_init.0, &[], &[]);
        ::protobuf::__internal::runtime::MiniTableInitPtr(super::helloworld__HelloRequest_msg_init.0)
      }).0
    }
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetArena for HelloRequest {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for HelloRequest {
  type Msg = HelloRequest;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloRequest> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloRequest {
  type Msg = HelloRequest;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloRequest> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for HelloRequestMut<'_> {
  type Msg = HelloRequest;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloRequest> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloRequestMut<'_> {
  type Msg = HelloRequest;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloRequest> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloRequestView<'_> {
  type Msg = HelloRequest;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloRequest> {
    self.inner.ptr()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetArena for HelloRequestMut<'_> {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}



// This variable must not be referenced except by protobuf generated
// code.
pub(crate) static mut helloworld__HelloReply_msg_init: ::protobuf::__internal::runtime::MiniTableInitPtr =
    ::protobuf::__internal::runtime::MiniTableInitPtr(::protobuf::__internal::runtime::MiniTablePtr::dangling());
#[allow(non_camel_case_types)]
pub struct HelloReply {
  inner: ::protobuf::__internal::runtime::OwnedMessageInner<HelloReply>
}

impl ::protobuf::Message for HelloReply {}

impl ::std::default::Default for HelloReply {
  fn default() -> Self {
    Self::new()
  }
}

impl ::std::fmt::Debug for HelloReply {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

// SAFETY:
// - `HelloReply` is `Sync` because it does not implement interior mutability.
//    Neither does `HelloReplyMut`.
unsafe impl Sync for HelloReply {}

// SAFETY:
// - `HelloReply` is `Send` because it uniquely owns its arena and does
//   not use thread-local data.
unsafe impl Send for HelloReply {}

impl ::protobuf::Proxied for HelloReply {
  type View<'msg> = HelloReplyView<'msg>;
}

impl ::protobuf::__internal::SealedInternal for HelloReply {}

impl ::protobuf::MutProxied for HelloReply {
  type Mut<'msg> = HelloReplyMut<'msg>;
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub struct HelloReplyView<'msg> {
  inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, HelloReply>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for HelloReplyView<'msg> {}

impl<'msg> ::protobuf::MessageView<'msg> for HelloReplyView<'msg> {
  type Message = HelloReply;
}

impl ::std::fmt::Debug for HelloReplyView<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl ::std::default::Default for HelloReplyView<'_> {
  fn default() -> HelloReplyView<'static> {
    ::protobuf::__internal::runtime::MessageViewInner::default().into()
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageViewInner<'msg, HelloReply>> for HelloReplyView<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageViewInner<'msg, HelloReply>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> HelloReplyView<'msg> {

  pub fn to_owned(&self) -> HelloReply {
    ::protobuf::IntoProxied::into_proxied(*self, ::protobuf::__internal::Private)
  }

  // message: optional string
  pub fn message(self) -> ::protobuf::View<'msg, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }

}

// SAFETY:
// - `HelloReplyView` is `Sync` because it does not support mutation.
unsafe impl Sync for HelloReplyView<'_> {}

// SAFETY:
// - `HelloReplyView` is `Send` because while its alive a `HelloReplyMut` cannot.
// - `HelloReplyView` does not use thread-local data.
unsafe impl Send for HelloReplyView<'_> {}

impl<'msg> ::protobuf::AsView for HelloReplyView<'msg> {
  type Proxied = HelloReply;
  fn as_view(&self) -> ::protobuf::View<'msg, HelloReply> {
    *self
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for HelloReplyView<'msg> {
  fn into_view<'shorter>(self) -> HelloReplyView<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

impl<'msg> ::protobuf::IntoProxied<HelloReply> for HelloReplyView<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> HelloReply {
    let mut dst = HelloReply::new();
    assert!(unsafe {
      dst.inner.ptr_mut().deep_copy(self.inner.ptr(), dst.inner.arena())
    });
    dst
  }
}

impl<'msg> ::protobuf::IntoProxied<HelloReply> for HelloReplyMut<'msg> {
  fn into_proxied(self, _private: ::protobuf::__internal::Private) -> HelloReply {
    ::protobuf::IntoProxied::into_proxied(::protobuf::IntoView::into_view(self), _private)
  }
}

impl ::protobuf::__internal::runtime::EntityType for HelloReply {
    type Tag = ::protobuf::__internal::runtime::MessageTag;
}

impl<'msg> ::protobuf::__internal::runtime::EntityType for HelloReplyView<'msg> {
    type Tag = ::protobuf::__internal::runtime::ViewProxyTag;
}

impl<'msg> ::protobuf::__internal::runtime::EntityType for HelloReplyMut<'msg> {
    type Tag = ::protobuf::__internal::runtime::MutProxyTag;
}

#[allow(dead_code)]
#[allow(non_camel_case_types)]
pub struct HelloReplyMut<'msg> {
  inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloReply>,
}

impl<'msg> ::protobuf::__internal::SealedInternal for HelloReplyMut<'msg> {}

impl<'msg> ::protobuf::MessageMut<'msg> for HelloReplyMut<'msg> {
  type Message = HelloReply;
}

impl ::std::fmt::Debug for HelloReplyMut<'_> {
  fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
    write!(f, "{}", ::protobuf::__internal::runtime::debug_string(self))
  }
}

impl<'msg> From<::protobuf::__internal::runtime::MessageMutInner<'msg, HelloReply>> for HelloReplyMut<'msg> {
  fn from(inner: ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloReply>) -> Self {
    Self { inner }
  }
}

#[allow(dead_code)]
impl<'msg> HelloReplyMut<'msg> {

  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private)
    -> ::protobuf::__internal::runtime::MessageMutInner<'msg, HelloReply> {
    self.inner
  }

  pub fn to_owned(&self) -> HelloReply {
    ::protobuf::AsView::as_view(self).to_owned()
  }

  // message: optional string
  pub fn message(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }
  pub fn set_message(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        0,
        val);
    }
  }

}

// SAFETY:
// - `HelloReplyMut` does not perform any shared mutation.
unsafe impl Send for HelloReplyMut<'_> {}

// SAFETY:
// - `HelloReplyMut` does not perform any shared mutation.
unsafe impl Sync for HelloReplyMut<'_> {}

impl<'msg> ::protobuf::AsView for HelloReplyMut<'msg> {
  type Proxied = HelloReply;
  fn as_view(&self) -> ::protobuf::View<'_, HelloReply> {
    HelloReplyView {
      inner: ::protobuf::__internal::runtime::MessageViewInner::view_of_mut(self.inner)
    }
  }
}

impl<'msg> ::protobuf::IntoView<'msg> for HelloReplyMut<'msg> {
  fn into_view<'shorter>(self) -> ::protobuf::View<'shorter, HelloReply>
  where
      'msg: 'shorter {
    HelloReplyView {
      inner: ::protobuf::__internal::runtime::MessageViewInner::view_of_mut(self.inner)
    }
  }
}

impl<'msg> ::protobuf::AsMut for HelloReplyMut<'msg> {
  type MutProxied = HelloReply;
  fn as_mut(&mut self) -> HelloReplyMut<'msg> {
    HelloReplyMut { inner: self.inner }
  }
}

impl<'msg> ::protobuf::IntoMut<'msg> for HelloReplyMut<'msg> {
  fn into_mut<'shorter>(self) -> HelloReplyMut<'shorter>
  where
      'msg: 'shorter {
    self
  }
}

#[allow(dead_code)]
impl HelloReply {
  pub fn new() -> Self {
    Self { inner: ::protobuf::__internal::runtime::OwnedMessageInner::<Self>::new() }
  }


  #[doc(hidden)]
  pub fn as_message_mut_inner(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessageMutInner<'_, HelloReply> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner)
  }

  pub fn as_view(&self) -> HelloReplyView<'_> {
    ::protobuf::__internal::runtime::MessageViewInner::view_of_owned(&self.inner).into()
  }

  pub fn as_mut(&mut self) -> HelloReplyMut<'_> {
    ::protobuf::__internal::runtime::MessageMutInner::mut_of_owned(&mut self.inner).into()
  }

  // message: optional string
  pub fn message(&self) -> ::protobuf::View<'_, ::protobuf::ProtoString> {
    let str_view = unsafe {
      self.inner.ptr().get_string_at_index(
        0, (b"").into()
      )
    };
    // SAFETY: The runtime doesn't require ProtoStr to be UTF-8.
    unsafe { ::protobuf::ProtoStr::from_utf8_unchecked(str_view.as_ref()) }
  }
  pub fn set_message(&mut self, val: impl ::protobuf::IntoProxied<::protobuf::ProtoString>) {
    unsafe {
      ::protobuf::__internal::runtime::message_set_string_field(
        ::protobuf::AsMut::as_mut(self).inner,
        0,
        val);
    }
  }

}  // impl HelloReply

impl ::std::ops::Drop for HelloReply {
  #[inline]
  fn drop(&mut self) {
  }
}

impl ::std::clone::Clone for HelloReply {
  fn clone(&self) -> Self {
    self.as_view().to_owned()
  }
}

impl ::protobuf::AsView for HelloReply {
  type Proxied = Self;
  fn as_view(&self) -> HelloReplyView<'_> {
    self.as_view()
  }
}

impl ::protobuf::AsMut for HelloReply {
  type MutProxied = Self;
  fn as_mut(&mut self) -> HelloReplyMut<'_> {
    self.as_mut()
  }
}

unsafe impl ::protobuf::__internal::runtime::AssociatedMiniTable for HelloReply {
  fn mini_table() -> ::protobuf::__internal::runtime::MiniTablePtr {
    static ONCE_LOCK: ::std::sync::OnceLock<::protobuf::__internal::runtime::MiniTableInitPtr> =
        ::std::sync::OnceLock::new();
    unsafe {
      ONCE_LOCK.get_or_init(|| {
        super::helloworld__HelloReply_msg_init.0 =
            ::protobuf::__internal::runtime::build_mini_table("$M1P");
        ::protobuf::__internal::runtime::link_mini_table(
            super::helloworld__HelloReply_msg_init.0, &[], &[]);
        ::protobuf::__internal::runtime::MiniTableInitPtr(super::helloworld__HelloReply_msg_init.0)
      }).0
    }
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetArena for HelloReply {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for HelloReply {
  type Msg = HelloReply;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloReply> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloReply {
  type Msg = HelloReply;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloReply> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtrMut for HelloReplyMut<'_> {
  type Msg = HelloReply;
  fn get_ptr_mut(&mut self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloReply> {
    self.inner.ptr_mut()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloReplyMut<'_> {
  type Msg = HelloReply;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloReply> {
    self.inner.ptr()
  }
}
unsafe impl ::protobuf::__internal::runtime::UpbGetMessagePtr for HelloReplyView<'_> {
  type Msg = HelloReply;
  fn get_ptr(&self, _private: ::protobuf::__internal::Private) -> ::protobuf::__internal::runtime::MessagePtr<HelloReply> {
    self.inner.ptr()
  }
}

unsafe impl ::protobuf::__internal::runtime::UpbGetArena for HelloReplyMut<'_> {
  fn get_arena(&mut self, _private: ::protobuf::__internal::Private) -> &::protobuf::__internal::runtime::Arena {
    self.inner.arena()
  }
}




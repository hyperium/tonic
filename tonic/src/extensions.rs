use std::fmt;

/// A type map of protocol extensions.
///
/// `Extensions` can be used by [`Interceptor`] and [`Request`] to store extra data derived from
/// the underlying protocol.
///
/// [`Interceptor`]: crate::Interceptor
/// [`Request`]: crate::Request
pub struct Extensions(http::Extensions);

impl Extensions {
    pub(crate) fn new() -> Self {
        Self(http::Extensions::new())
    }

    /// Insert a type into this `Extensions`.
    ///
    /// If a extension of this type already existed, it will
    /// be returned.
    #[inline]
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) -> Option<T> {
        self.0.insert(val)
    }

    /// Get a reference to a type previously inserted on this `Extensions`.
    #[inline]
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.0.get()
    }

    /// Get a mutable reference to a type previously inserted on this `Extensions`.
    #[inline]
    pub fn get_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.0.get_mut()
    }

    /// Remove a type from this `Extensions`.
    ///
    /// If a extension of this type existed, it will be returned.
    #[inline]
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.0.remove()
    }

    /// Clear the `Extensions` of all inserted extensions.
    #[inline]
    pub fn clear(&mut self) {
        self.0.clear()
    }

    #[inline]
    pub(crate) fn from_http(http: http::Extensions) -> Self {
        Self(http)
    }

    #[inline]
    pub(crate) fn into_http(self) -> http::Extensions {
        self.0
    }
}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions").finish()
    }
}

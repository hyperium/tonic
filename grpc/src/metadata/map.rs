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

use std::marker::PhantomData;

use http::HeaderName;
use http::HeaderValue;

pub(crate) use self::as_encoding_agnostic_metadata_key::AsEncodingAgnosticMetadataKey;
pub(crate) use self::as_metadata_key::AsMetadataKey;
pub(crate) use self::into_metadata_key::IntoMetadataKey;
use super::encoding::Ascii;
use super::encoding::Binary;
use super::encoding::ValueEncoding;
use super::key::MetadataKey;
use super::value::MetadataValue;
use crate::metadata::encoding::value_encoding::Sealed;
use crate::metadata::value::UnencodedHeaderValue;

/// A set of gRPC custom metadata entries.
///
/// # Examples
///
/// Basic usage
///
/// ```
/// # use grpc::metadata::*;
/// let mut map = MetadataMap::new();
///
/// map.insert("x-host", "example.com".parse().unwrap());
/// map.insert("x-number", "123".parse().unwrap());
/// map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"[binary data]"));
///
/// assert!(map.contains_key("x-host"));
/// assert!(!map.contains_key("x-location"));
///
/// assert_eq!(map.get("x-host").unwrap(), "example.com");
///
/// map.remove("x-host");
///
/// assert!(!map.contains_key("x-host"));
/// ```
#[derive(Clone, Debug, Default)]
pub struct MetadataMap {
    headers: Vec<(HeaderName, UnencodedHeaderValue)>,
}

/// `MetadataMap` entry iterator.
///
/// Yields `KeyAndValueRef` values. The same header name may be yielded
/// more than once if it has more than one associated value.
#[derive(Debug)]
pub struct Iter<'a> {
    inner: std::slice::Iter<'a, (HeaderName, UnencodedHeaderValue)>,
}

/// Reference to a key and an associated value in a `MetadataMap`. It can point
/// to either an ascii or a binary ("*-bin") key.
#[derive(Debug)]
pub enum KeyAndValueRef<'a> {
    /// An ascii metadata key and value.
    Ascii(&'a MetadataKey<Ascii>, &'a MetadataValue<Ascii>),
    /// A binary metadata key and value.
    Binary(&'a MetadataKey<Binary>, &'a MetadataValue<Binary>),
}

#[derive(Debug)]
pub struct ValueDrain<'a, VE: ValueEncoding> {
    inner: std::vec::Drain<'a, (HeaderName, UnencodedHeaderValue)>,
    _phantom: PhantomData<&'a VE>,
}

/// Reference to a key in a `MetadataMap`. It can point
/// to either an ascii or a binary ("*-bin") key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// An ascii metadata key and value.
    Ascii(MetadataKey<Ascii>),
    /// A binary metadata key and value.
    Binary(MetadataKey<Binary>),
}

/// An iterator of all values associated with a single metadata key.
#[derive(Debug)]
pub struct ValueIter<'a, VE: ValueEncoding> {
    inner: std::slice::Iter<'a, (HeaderName, UnencodedHeaderValue)>,
    key: Option<MetadataKey<VE>>,
}

/// A view to all values stored in a single entry.
///
/// This struct is returned by `MetadataMap::get_all` and
/// `MetadataMap::get_all_bin`.
#[derive(Debug)]
pub struct GetAll<'a, VE: ValueEncoding> {
    map: &'a MetadataMap,
    key: Option<MetadataKey<VE>>,
}

// ===== impl MetadataMap =====

impl MetadataMap {
    // Headers reserved by the gRPC protocol.
    pub(crate) const GRPC_RESERVED_HEADERS: [HeaderName; 5] = [
        HeaderName::from_static("te"),
        HeaderName::from_static("content-type"),
        HeaderName::from_static("grpc-message"),
        HeaderName::from_static("grpc-message-type"),
        HeaderName::from_static("grpc-status"),
    ];

    /// Create an empty `MetadataMap`.
    ///
    /// The map will be created without any capacity. This function will not
    /// allocate.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let map = MetadataMap::new();
    ///
    /// assert!(map.is_empty());
    /// assert_eq!(0, map.capacity());
    /// ```
    pub fn new() -> Self {
        MetadataMap::with_capacity(0)
    }

    /// Convert an HTTP HeaderMap to a MetadataMap
    pub fn from_headers(headers: http::HeaderMap) -> Self {
        let mut ret = Vec::with_capacity(headers.len());
        let mut current_key: Option<HeaderName> = None;

        for (key, value) in headers {
            if let Some(k) = key {
                current_key = Some(k);
            }

            // If we don't have a key yet, skip to the next iteration.
            let Some(k) = current_key.as_ref() else {
                continue;
            };
            let key_str = k.as_str();

            if Ascii::is_valid_key(key_str) {
                if let Ok(mut mv) = MetadataValue::<Ascii>::try_from(value.as_bytes()) {
                    mv.set_sensitive(value.is_sensitive());
                    ret.push((k.clone(), mv.inner));
                }
            } else if Binary::is_valid_key(key_str) {
                if let Ok(b) = Binary::decode(value.as_bytes()) {
                    let mut mv = unsafe { MetadataValue::<Binary>::from_shared_unchecked(b) };
                    mv.set_sensitive(value.is_sensitive());
                    ret.push((k.clone(), mv.inner));
                }
            }
        }

        Self { headers: ret }
    }

    /// Convert a MetadataMap into a HTTP HeaderMap
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert("x-host", "example.com".parse().unwrap());
    ///
    /// let http_map = map.into_headers();
    ///
    /// assert_eq!(http_map.get("x-host").unwrap(), "example.com");
    /// ```
    pub fn into_headers(self) -> http::HeaderMap {
        let mut ret = http::HeaderMap::with_capacity(self.capacity());
        for (key, value) in self.headers {
            let bytes = if Ascii::is_valid_key(key.as_str()) {
                MetadataValue::<Ascii>::encode(value.data)
            } else {
                MetadataValue::<Binary>::encode(value.data)
            };
            // gRPC's validation is stricter than HTTP/2.
            unsafe {
                ret.append(key, HeaderValue::from_maybe_shared_unchecked(bytes));
            }
        }
        ret
    }

    pub(crate) fn into_sanitized_headers(self) -> http::HeaderMap {
        let mut headers = self.into_headers();
        for r in &Self::GRPC_RESERVED_HEADERS {
            headers.remove(r);
        }
        headers
    }

    /// Create an empty `MetadataMap` with the specified capacity.
    ///
    /// The returned map will allocate internal storage in order to hold about
    /// `capacity` elements without reallocating. However, this is a "best
    /// effort" as there are usage patterns that could cause additional
    /// allocations before `capacity` metadata entries are stored in the map.
    ///
    /// More capacity than requested may be allocated.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let map: MetadataMap = MetadataMap::with_capacity(10);
    ///
    /// assert!(map.is_empty());
    /// assert!(map.capacity() >= 10);
    /// ```
    pub fn with_capacity(capacity: usize) -> MetadataMap {
        MetadataMap {
            headers: Vec::with_capacity(capacity),
        }
    }

    /// Returns the number of metadata entries (ascii and binary) stored in the
    /// map.
    ///
    /// This number represents the total number of **values** stored in the map.
    /// This number can be greater than or equal to the number of **keys**
    /// stored given that a single key may have more than one associated value.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// assert_eq!(0, map.len());
    ///
    /// map.insert("x-host-ip", "127.0.0.1".parse().unwrap());
    /// map.insert_bin("x-host-name-bin", MetadataValue::from_bytes(b"localhost"));
    ///
    /// assert_eq!(2, map.len());
    ///
    /// map.append("x-host-ip", "text/html".parse().unwrap());
    ///
    /// assert_eq!(3, map.len());
    /// ```
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    /// Returns true if the map contains no elements.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// assert!(map.is_empty());
    ///
    /// map.insert("x-host", "hello.world".parse().unwrap());
    ///
    /// assert!(!map.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Clears the map, removing all key-value pairs. Keeps the allocated memory
    /// for reuse.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert("x-host", "hello.world".parse().unwrap());
    ///
    /// map.clear();
    /// assert!(map.is_empty());
    /// assert!(map.capacity() > 0);
    /// ```
    pub fn clear(&mut self) {
        self.headers.clear();
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all key-value pairs `(k, v)` such that
    /// `f(KeyAndValueRef)` returns `false`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// map.insert("x-host", "hello".parse().unwrap());
    /// map.insert("x-number", "123".parse().unwrap());
    /// map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"world"));
    ///
    /// map.retain(|entry| {
    ///     match entry {
    ///         KeyAndValueRef::Ascii(key, _) => key == "x-host",
    ///         _ => false,
    ///     }
    /// });
    ///
    /// assert_eq!(map.len(), 1);
    /// assert!(map.contains_key("x-host"));
    /// assert!(!map.contains_key("x-number"));
    /// assert!(!map.contains_key("trace-proto-bin"));
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(KeyAndValueRef<'_>) -> bool,
    {
        self.headers.retain(|(name, value)| {
            let key_and_value = if !name.as_str().ends_with("-bin") {
                KeyAndValueRef::Ascii(
                    MetadataKey::unchecked_from_header_name_ref(name),
                    MetadataValue::unchecked_from_header_value_ref(value),
                )
            } else {
                KeyAndValueRef::Binary(
                    MetadataKey::unchecked_from_header_name_ref(name),
                    MetadataValue::unchecked_from_header_value_ref(value),
                )
            };
            f(key_and_value)
        });
    }

    /// Returns the number of custom metadata entries the map can hold without
    /// reallocating.
    ///
    /// This number is an approximation as certain usage patterns could cause
    /// additional allocations before the returned capacity is filled.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// assert_eq!(0, map.capacity());
    ///
    /// map.insert("x-host", "hello.world".parse().unwrap());
    /// assert_eq!(6, map.capacity());
    /// ```
    pub fn capacity(&self) -> usize {
        self.headers.capacity()
    }

    /// Reserves capacity for at least `additional` more custom metadata to be
    /// inserted into the `MetadataMap`.
    ///
    /// The metadata map may reserve more space to avoid frequent reallocations.
    /// Like with `with_capacity`, this will be a "best effort" to avoid
    /// allocations until `additional` more custom metadata is inserted. Certain
    /// usage patterns could cause additional allocations before the number is
    /// reached.
    ///
    /// # Panics
    ///
    /// Panics if the new allocation size overflows `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.reserve(10);
    /// # map.insert("x-host", "bar".parse().unwrap());
    /// ```
    pub fn reserve(&mut self, additional: usize) {
        self.headers.reserve(additional);
    }

    /// Returns a reference to the value associated with the key. This method
    /// is for ascii metadata entries (those whose names don't end with
    /// "-bin"). For binary entries, use get_bin.
    ///
    /// If there are multiple values associated with the key, then the first one
    /// is returned. Use `get_all` to get all values associated with a given
    /// key. Returns `None` if there are no values associated with the key.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.get("x-host").is_none());
    ///
    /// map.insert("x-host", "hello".parse().unwrap());
    /// assert_eq!(map.get("x-host").unwrap(), &"hello");
    /// assert_eq!(map.get("x-host").unwrap(), &"hello");
    ///
    /// map.append("x-host", "world".parse().unwrap());
    /// assert_eq!(map.get("x-host").unwrap(), &"hello");
    ///
    /// // Attempting to read a key of the wrong type fails by not
    /// // finding anything.
    /// map.append_bin("host-bin", MetadataValue::from_bytes(b"world"));
    /// assert!(map.get("host-bin").is_none());
    /// assert!(map.get("host-bin".to_string()).is_none());
    /// assert!(map.get(&("host-bin".to_string())).is_none());
    ///
    /// // Attempting to read an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.get("host{}bin").is_none());
    /// assert!(map.get("host{}bin".to_string()).is_none());
    /// assert!(map.get(&("host{}bin".to_string())).is_none());
    /// ```
    pub fn get<K>(&self, key: K) -> Option<&MetadataValue<Ascii>>
    where
        K: AsMetadataKey<Ascii>,
    {
        key.get(self)
    }

    /// Like get, but for Binary keys (for example "trace-proto-bin").
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.get_bin("trace-proto-bin").is_none());
    ///
    /// map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"hello"));
    /// assert_eq!(map.get_bin("trace-proto-bin").unwrap(), &"hello");
    /// assert_eq!(map.get_bin("trace-proto-bin").unwrap(), &"hello");
    ///
    /// map.append_bin("trace-proto-bin", MetadataValue::from_bytes(b"world"));
    /// assert_eq!(map.get_bin("trace-proto-bin").unwrap(), &"hello");
    ///
    /// // Attempting to read a key of the wrong type fails by not
    /// // finding anything.
    /// map.append("host", "world".parse().unwrap());
    /// assert!(map.get_bin("host").is_none());
    /// assert!(map.get_bin("host".to_string()).is_none());
    /// assert!(map.get_bin(&("host".to_string())).is_none());
    ///
    /// // Attempting to read an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.get_bin("host{}-bin").is_none());
    /// assert!(map.get_bin("host{}-bin".to_string()).is_none());
    /// assert!(map.get_bin(&("host{}-bin".to_string())).is_none());
    /// ```
    pub fn get_bin<K>(&self, key: K) -> Option<&MetadataValue<Binary>>
    where
        K: AsMetadataKey<Binary>,
    {
        key.get(self)
    }

    /// Returns a view of all values associated with a key. This method is for
    /// ascii metadata entries (those whose names don't end with "-bin"). For
    /// binary entries, use get_all_bin.
    ///
    /// The returned view does not incur any allocations and allows iterating
    /// the values associated with the key.  See [`GetAll`] for more details.
    /// Returns `None` if there are no values associated with the key.
    ///
    /// [`GetAll`]: struct.GetAll.html
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// map.insert("x-host", "hello".parse().unwrap());
    /// map.append("x-host", "goodbye".parse().unwrap());
    ///
    /// {
    ///     let view = map.get_all("x-host");
    ///
    ///     let mut iter = view.iter();
    ///     assert_eq!(&"hello", iter.next().unwrap());
    ///     assert_eq!(&"goodbye", iter.next().unwrap());
    ///     assert!(iter.next().is_none());
    /// }
    ///
    /// // Attempting to read a key of the wrong type fails by not
    /// // finding anything.
    /// map.append_bin("host-bin", MetadataValue::from_bytes(b"world"));
    /// assert!(map.get_all("host-bin").iter().next().is_none());
    /// assert!(map.get_all("host-bin".to_string()).iter().next().is_none());
    /// assert!(map.get_all(&("host-bin".to_string())).iter().next().is_none());
    ///
    /// // Attempting to read an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.get_all("host{}").iter().next().is_none());
    /// assert!(map.get_all("host{}".to_string()).iter().next().is_none());
    /// assert!(map.get_all(&("host{}".to_string())).iter().next().is_none());
    /// ```
    pub fn get_all<K>(&self, key: K) -> GetAll<'_, Ascii>
    where
        K: AsMetadataKey<Ascii>,
    {
        key.get_all(self)
    }

    /// Like get_all, but for Binary keys (for example "trace-proto-bin").
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"hello"));
    /// map.append_bin("trace-proto-bin", MetadataValue::from_bytes(b"goodbye"));
    ///
    /// {
    ///     let view = map.get_all_bin("trace-proto-bin");
    ///
    ///     let mut iter = view.iter();
    ///     assert_eq!(&"hello", iter.next().unwrap());
    ///     assert_eq!(&"goodbye", iter.next().unwrap());
    ///     assert!(iter.next().is_none());
    /// }
    ///
    /// // Attempting to read a key of the wrong type fails by not
    /// // finding anything.
    /// map.append("host", "world".parse().unwrap());
    /// assert!(map.get_all_bin("host").iter().next().is_none());
    /// assert!(map.get_all_bin("host".to_string()).iter().next().is_none());
    /// assert!(map.get_all_bin(&("host".to_string())).iter().next().is_none());
    ///
    /// // Attempting to read an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.get_all_bin("host{}-bin").iter().next().is_none());
    /// assert!(map.get_all_bin("host{}-bin".to_string()).iter().next().is_none());
    /// assert!(map.get_all_bin(&("host{}-bin".to_string())).iter().next().is_none());
    /// ```
    pub fn get_all_bin<K>(&self, key: K) -> GetAll<'_, Binary>
    where
        K: AsMetadataKey<Binary>,
    {
        key.get_all(self)
    }

    /// Returns true if the map contains a value for the specified key. This
    /// method works for both ascii and binary entries.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(!map.contains_key("x-host"));
    ///
    /// map.append_bin("host-bin", MetadataValue::from_bytes(b"world"));
    /// map.insert("x-host", "world".parse().unwrap());
    ///
    /// // contains_key works for both Binary and Ascii keys:
    /// assert!(map.contains_key("x-host"));
    /// assert!(map.contains_key("host-bin"));
    ///
    /// // contains_key returns false for invalid keys:
    /// assert!(!map.contains_key("x{}host"));
    /// ```
    pub fn contains_key<K>(&self, key: K) -> bool
    where
        K: AsEncodingAgnosticMetadataKey,
    {
        key.contains_key(self)
    }

    /// An iterator visiting all key-value pairs (both ascii and binary).
    ///
    /// The iteration order is arbitrary, but consistent across platforms for
    /// the same crate version. Each key will be yielded once per associated
    /// value. So, if a key has 3 associated values, it will be yielded 3 times.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    ///
    /// map.insert("x-word", "hello".parse().unwrap());
    /// map.append("x-word", "goodbye".parse().unwrap());
    /// map.insert("x-number", "123".parse().unwrap());
    ///
    /// for key_and_value in map.iter() {
    ///     match key_and_value {
    ///         KeyAndValueRef::Ascii(ref key, ref value) =>
    ///             println!("Ascii: {:?}: {:?}", key, value),
    ///         KeyAndValueRef::Binary(ref key, ref value) =>
    ///             println!("Binary: {:?}: {:?}", key, value),
    ///     }
    /// }
    /// ```
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.headers.iter(),
        }
    }

    /// Inserts an ascii key-value pair into the map. To insert a binary entry,
    /// use `insert_bin`.
    ///
    /// This method panics when the given key is a string and it cannot be
    /// converted to a `MetadataKey<Ascii>`.
    ///
    /// If the map did not previously have this key present, then `None` is
    /// returned.
    ///
    /// If the map did have this key present, the new value is associated with
    /// the key and all previous values are removed. **Note** that only a single
    /// one of the previous values is returned. If there are multiple values
    /// that have been previously associated with the key, then the first one is
    /// returned. See `insert_mult` on `OccupiedEntry` for an API that returns
    /// all values.
    ///
    /// The key is not updated, though; this matters for types that can be `==`
    /// without being identical.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.insert("x-host", "world".parse().unwrap()).is_none());
    /// assert!(!map.is_empty());
    ///
    /// let mut prev = map.insert("x-host", "earth".parse().unwrap()).unwrap();
    /// assert_eq!("world", prev);
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to insert a key that is not valid panics.
    /// map.insert("x{}host", "world".parse().unwrap());
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to insert a key that is binary panics (use insert_bin).
    /// map.insert("x-host-bin", "world".parse().unwrap());
    /// ```
    pub fn insert<K>(&mut self, key: K, val: MetadataValue<Ascii>) -> Option<MetadataValue<Ascii>>
    where
        K: IntoMetadataKey<Ascii>,
    {
        key.insert(self, val)
    }

    /// Like insert, but for Binary keys (for example "trace-proto-bin").
    ///
    /// This method panics when the given key is a string and it cannot be
    /// converted to a `MetadataKey<Binary>`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"world")).is_none());
    /// assert!(!map.is_empty());
    ///
    /// let mut prev = map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"earth")).unwrap();
    /// assert_eq!("world", prev);
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::default();
    /// // Attempting to add a binary metadata entry with an invalid name
    /// map.insert_bin("trace-proto", MetadataValue::from_bytes(b"hello")); // This line panics!
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to insert a key that is not valid panics.
    /// map.insert_bin("x{}host-bin", MetadataValue::from_bytes(b"world")); // This line panics!
    /// ```
    pub fn insert_bin<K>(
        &mut self,
        key: K,
        val: MetadataValue<Binary>,
    ) -> Option<MetadataValue<Binary>>
    where
        K: IntoMetadataKey<Binary>,
    {
        key.insert(self, val)
    }

    /// Inserts an ascii key-value pair into the map. To insert a binary entry,
    /// use `append_bin`.
    ///
    /// This method panics when the given key is a string and it cannot be
    /// converted to a `MetadataKey<Ascii>`.
    ///
    /// If the map did not previously have this key present, then `false` is
    /// returned.
    ///
    /// If the map did have this key present, the new value is pushed to the end
    /// of the list of values currently associated with the key. The key is not
    /// updated, though; this matters for types that can be `==` without being
    /// identical.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.insert("x-host", "world".parse().unwrap()).is_none());
    /// assert!(!map.is_empty());
    ///
    /// map.append("x-host", "earth".parse().unwrap());
    ///
    /// let values = map.get_all("x-host");
    /// let mut i = values.iter();
    /// assert_eq!("world", *i.next().unwrap());
    /// assert_eq!("earth", *i.next().unwrap());
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to append a key that is not valid panics.
    /// map.append("x{}host", "world".parse().unwrap()); // This line panics!
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to append a key that is binary panics (use append_bin).
    /// map.append("x-host-bin", "world".parse().unwrap()); // This line panics!
    /// ```
    pub fn append<K>(&mut self, key: K, value: MetadataValue<Ascii>)
    where
        K: IntoMetadataKey<Ascii>,
    {
        key.append(self, value);
    }

    /// Like append, but for binary keys (for example "trace-proto-bin").
    ///
    /// This method panics when the given key is a string and it cannot be
    /// converted to a `MetadataKey<Binary>`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// assert!(map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"world")).is_none());
    /// assert!(!map.is_empty());
    ///
    /// map.append_bin("trace-proto-bin", MetadataValue::from_bytes(b"earth"));
    ///
    /// let values = map.get_all_bin("trace-proto-bin");
    /// let mut i = values.iter();
    /// assert_eq!("world", *i.next().unwrap());
    /// assert_eq!("earth", *i.next().unwrap());
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to append a key that is not valid panics.
    /// map.append_bin("x{}host-bin", MetadataValue::from_bytes(b"world")); // This line panics!
    /// ```
    ///
    /// ```should_panic
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// // Trying to append a key that is ascii panics (use append).
    /// map.append_bin("x-host", MetadataValue::from_bytes(b"world")); // This line panics!
    /// ```
    pub fn append_bin<K>(&mut self, key: K, value: MetadataValue<Binary>)
    where
        K: IntoMetadataKey<Binary>,
    {
        key.append(self, value);
    }

    /// Removes an ascii key from the map, returning the value associated with
    /// the key. To remove a binary key, use `remove_bin`.
    ///
    /// Returns `None` if the map does not contain the key. If there are
    /// multiple values associated with the key, then the first one is returned.
    /// See `remove_entry_mult` on `OccupiedEntry` for an API that yields all
    /// values.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert("x-host", "hello.world".parse().unwrap());
    ///
    /// let prev = map.remove("x-host").unwrap();
    /// assert_eq!("hello.world", prev);
    ///
    /// assert!(map.remove("x-host").is_none());
    ///
    /// // Attempting to remove a key of the wrong type fails by not
    /// // finding anything.
    /// map.append_bin("host-bin", MetadataValue::from_bytes(b"world"));
    /// assert!(map.remove("host-bin").is_none());
    /// assert!(map.remove("host-bin".to_string()).is_none());
    /// assert!(map.remove(&("host-bin".to_string())).is_none());
    ///
    /// // Attempting to remove an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.remove("host{}").is_none());
    /// assert!(map.remove("host{}".to_string()).is_none());
    /// assert!(map.remove(&("host{}".to_string())).is_none());
    /// ```
    pub fn remove<K>(&mut self, key: K) -> Option<MetadataValue<Ascii>>
    where
        K: AsMetadataKey<Ascii>,
    {
        key.remove(self)
    }

    /// Like remove, but for Binary keys (for example "trace-proto-bin").
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"hello.world"));
    ///
    /// let prev = map.remove_bin("trace-proto-bin").unwrap();
    /// assert_eq!("hello.world", prev);
    ///
    /// assert!(map.remove_bin("trace-proto-bin").is_none());
    ///
    /// // Attempting to remove a key of the wrong type fails by not
    /// // finding anything.
    /// map.append("host", "world".parse().unwrap());
    /// assert!(map.remove_bin("host").is_none());
    /// assert!(map.remove_bin("host".to_string()).is_none());
    /// assert!(map.remove_bin(&("host".to_string())).is_none());
    ///
    /// // Attempting to remove an invalid key string fails by not
    /// // finding anything.
    /// assert!(map.remove_bin("host{}-bin").is_none());
    /// assert!(map.remove_bin("host{}-bin".to_string()).is_none());
    /// assert!(map.remove_bin(&("host{}-bin".to_string())).is_none());
    /// ```
    pub fn remove_bin<K>(&mut self, key: K) -> Option<MetadataValue<Binary>>
    where
        K: AsMetadataKey<Binary>,
    {
        key.remove(self)
    }

    /// Removes all values for the given key and returns a draining iterator over them.
    ///
    /// **Note:** While the relative order of the remaining elements in the map
    /// is preserved, the order of the yielded elements is not guaranteed to
    /// match their original insertion order.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert("x-host", "example.com".parse().unwrap());
    /// map.append("x-host", "another.com".parse().unwrap());
    ///
    /// let values: Vec<_> = map.remove_all("x-host").collect();
    /// assert_eq!(2, values.len());
    /// ```
    // TODO: Switch to `Vec::extract_if` once the MSRV reaches 1.87. This will
    // allow us to maintain the original insertion order of the removed elements.
    pub fn remove_all<K>(&mut self, key: K) -> ValueDrain<'_, Ascii>
    where
        K: AsMetadataKey<Ascii>,
    {
        key.remove_all(self)
    }

    /// Removes all entries matching the given binary key and returns a
    /// draining iterator.
    ///
    /// This is the binary-key equivalent of [`remove_all`].
    ///
    /// **Note:** While the order of the remaining elements is preserved, the
    /// order of the yielded elements is not guaranteed to match their original
    /// insertion order.
    ///
    /// [`remove_all`]: Self::remove_all
    // TODO: Switch to `Vec::extract_if` once the MSRV reaches 1.87. This will
    // allow us to maintain the original insertion order of the removed elements.
    pub fn remove_all_bin<K>(&mut self, key: K) -> ValueDrain<'_, Binary>
    where
        K: AsMetadataKey<Binary>,
    {
        key.remove_all(self)
    }

    pub(crate) fn merge(&mut self, other: MetadataMap) {
        self.headers.extend(other.headers);
    }
}

// ===== impl Iter =====

impl<'a> Iterator for Iter<'a> {
    type Item = KeyAndValueRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(name, value)| {
            if !name.as_str().ends_with("-bin") {
                KeyAndValueRef::Ascii(
                    MetadataKey::unchecked_from_header_name_ref(name),
                    MetadataValue::unchecked_from_header_value_ref(value),
                )
            } else {
                KeyAndValueRef::Binary(
                    MetadataKey::unchecked_from_header_name_ref(name),
                    MetadataValue::unchecked_from_header_value_ref(value),
                )
            }
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

// ===== impl ValueDrain =====

impl<VE: ValueEncoding> Iterator for ValueDrain<'_, VE> {
    type Item = MetadataValue<VE>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|(_, v)| MetadataValue::unchecked_from_header_value(v))
    }
}

// ===== impl ValueIter =====

impl<'a, VE: ValueEncoding> Iterator for ValueIter<'a, VE>
where
    VE: 'a,
{
    type Item = &'a MetadataValue<VE>;

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.key.as_ref()?;
        for (k, value) in self.inner.by_ref() {
            if k == key.inner {
                return Some(MetadataValue::unchecked_from_header_value_ref(value));
            }
        }
        None
    }
}

// ===== impl GetAll =====

impl<'a, VE: ValueEncoding> GetAll<'a, VE> {
    /// Returns an iterator visiting all values associated with the entry.
    ///
    /// Values are iterated in insertion order.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::metadata::*;
    /// let mut map = MetadataMap::new();
    /// map.insert("x-host", "hello.world".parse().unwrap());
    /// map.append("x-host", "hello.earth".parse().unwrap());
    ///
    /// let values = map.get_all("x-host");
    /// let mut iter = values.iter();
    /// assert_eq!(&"hello.world", iter.next().unwrap());
    /// assert_eq!(&"hello.earth", iter.next().unwrap());
    /// assert!(iter.next().is_none());
    /// ```
    pub fn iter(&self) -> ValueIter<'a, VE> {
        ValueIter {
            inner: self.map.headers.iter(),
            key: self.key.clone(),
        }
    }
}

impl<VE: ValueEncoding> PartialEq for GetAll<'_, VE> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl<'a, VE: ValueEncoding> IntoIterator for GetAll<'a, VE>
where
    VE: 'a,
{
    type Item = &'a MetadataValue<VE>;
    type IntoIter = ValueIter<'a, VE>;

    fn into_iter(self) -> ValueIter<'a, VE> {
        self.iter()
    }
}

impl<'a, 'b: 'a, VE: ValueEncoding> IntoIterator for &'b GetAll<'a, VE> {
    type Item = &'a MetadataValue<VE>;
    type IntoIter = ValueIter<'a, VE>;

    fn into_iter(self) -> ValueIter<'a, VE> {
        self.iter()
    }
}

// ===== impl IntoMetadataKey / AsMetadataKey =====

mod into_metadata_key {
    use super::MetadataMap;
    use super::MetadataValue;
    use super::ValueEncoding;
    use crate::metadata::key::MetadataKey;

    /// A marker trait used to identify values that can be used as insert keys
    /// to a `MetadataMap`.
    pub trait IntoMetadataKey<VE: ValueEncoding>: Sealed<VE> {}

    // All methods are on this pub(super) trait, instead of `IntoMetadataKey`,
    // so that they aren't publicly exposed to the world.
    //
    // Being on the `IntoMetadataKey` trait would mean users could call
    // `"host".insert(&mut map, "localhost")`.
    //
    // Ultimately, this allows us to adjust the signatures of these methods
    // without breaking any external crate.
    pub trait Sealed<VE: ValueEncoding> {
        #[doc(hidden)]
        fn insert(self, map: &mut MetadataMap, val: MetadataValue<VE>)
            -> Option<MetadataValue<VE>>;

        #[doc(hidden)]
        fn append(self, map: &mut MetadataMap, val: MetadataValue<VE>);
    }

    // ==== impls ====

    impl<VE: ValueEncoding> Sealed<VE> for MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn insert(
            self,
            map: &mut MetadataMap,
            val: MetadataValue<VE>,
        ) -> Option<MetadataValue<VE>> {
            let key = self.inner;
            // Wrap val so we can move it out exactly once when we find a match
            let mut val_wrapper = Some(val.inner);
            let mut ret = None;

            let mut write_idx = 0;
            let len = map.headers.len();

            for read_idx in 0..len {
                // Check if keys match
                if map.headers[read_idx].0 == key {
                    if ret.is_none() {
                        // Found the first match.

                        // Swap values in-place.
                        // This moves the old value into `old_val` (no clone needed)
                        // and puts the new value into the vector.
                        let new_val = val_wrapper
                            .take()
                            .expect("Value should exist for first match");
                        let old_val = std::mem::replace(&mut map.headers[write_idx].1, new_val);

                        ret = Some(MetadataValue::unchecked_from_header_value(old_val));

                        // Keep this element
                        write_idx += 1;
                    } else {
                        // Found a subsequent match (Duplicate):
                        // Do not increment write_idx. This effectively removes the element
                        // by allowing it to be overwritten by the next valid element.
                    }
                } else {
                    // Not a match.
                    // Move to write position to keep list compact
                    if read_idx != write_idx {
                        map.headers.swap(read_idx, write_idx);
                    }
                    write_idx += 1;
                }
            }

            // Truncate the vector to the new length (removing duplicates/gaps)
            map.headers.truncate(write_idx);

            // If we never found a match, push the new entry now.
            if ret.is_none() {
                map.headers.push((key, val_wrapper.take().unwrap()));
            }

            ret
        }

        #[doc(hidden)]
        #[inline]
        fn append(self, map: &mut MetadataMap, val: MetadataValue<VE>) {
            map.headers.push((self.inner, val.inner));
        }
    }

    impl<VE: ValueEncoding> IntoMetadataKey<VE> for MetadataKey<VE> {}

    impl<VE: ValueEncoding> Sealed<VE> for &MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn insert(
            self,
            map: &mut MetadataMap,
            val: MetadataValue<VE>,
        ) -> Option<MetadataValue<VE>> {
            // Wrap val so we can move it out exactly once when we find a match
            let mut val_wrapper = Some(val.inner);
            let mut ret = None;

            let mut write_idx = 0;
            let len = map.headers.len();

            for read_idx in 0..len {
                // Check if keys match
                if map.headers[read_idx].0 == self.inner {
                    if ret.is_none() {
                        // Found the first match.

                        // Swap values in-place.
                        // This moves the old value into `old_val` (no clone needed)
                        // and puts the new value into the vector.
                        let new_val = val_wrapper
                            .take()
                            .expect("Value should exist for first match");
                        let old_val = std::mem::replace(&mut map.headers[write_idx].1, new_val);

                        ret = Some(MetadataValue::unchecked_from_header_value(old_val));

                        // Keep this element
                        write_idx += 1;
                    } else {
                        // Found a subsequent match (Duplicate):
                        // Do not increment write_idx. This effectively removes the element
                        // by allowing it to be overwritten by the next valid element.
                    }
                } else {
                    // Not a match.
                    // Move to write position to keep list compact
                    if read_idx != write_idx {
                        map.headers.swap(read_idx, write_idx);
                    }
                    write_idx += 1;
                }
            }

            // Truncate the vector to the new length (removing duplicates/gaps)
            map.headers.truncate(write_idx);

            // If we never found a match, push the new entry now.
            if ret.is_none() {
                map.headers
                    .push((self.inner.clone(), val_wrapper.take().unwrap()));
            }

            ret
        }
        #[doc(hidden)]
        #[inline]
        fn append(self, map: &mut MetadataMap, val: MetadataValue<VE>) {
            map.headers.push((self.inner.clone(), val.inner));
        }
    }

    impl<VE: ValueEncoding> IntoMetadataKey<VE> for &MetadataKey<VE> {}

    impl<VE: ValueEncoding> Sealed<VE> for &'static str {
        #[doc(hidden)]
        #[inline]
        fn insert(
            self,
            map: &mut MetadataMap,
            val: MetadataValue<VE>,
        ) -> Option<MetadataValue<VE>> {
            // Perform name validation
            let key = MetadataKey::<VE>::from_static(self);
            key.insert(map, val)
        }
        #[doc(hidden)]
        #[inline]
        fn append(self, map: &mut MetadataMap, val: MetadataValue<VE>) {
            // Perform name validation
            let key = MetadataKey::<VE>::from_static(self);
            key.append(map, val);
        }
    }

    impl<VE: ValueEncoding> IntoMetadataKey<VE> for &'static str {}
}

mod as_metadata_key {
    use std::marker::PhantomData;

    use super::GetAll;
    use super::MetadataMap;
    use super::MetadataValue;
    use super::ValueDrain;
    use super::ValueEncoding;
    use crate::metadata::key::MetadataKey;

    /// A marker trait used to identify values that can be used as search keys
    /// to a `MetadataMap`.
    pub trait AsMetadataKey<VE: ValueEncoding>: Sealed<VE> {}

    // All methods are on this pub(super) trait, instead of `AsMetadataKey`,
    // so that they aren't publicly exposed to the world.
    //
    // Being on the `AsMetadataKey` trait would mean users could call
    // `"host".find(&map)`.
    //
    // Ultimately, this allows us to adjust the signatures of these methods
    // without breaking any external crate.
    pub trait Sealed<VE: ValueEncoding> {
        #[doc(hidden)]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>>;

        #[doc(hidden)]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE>;

        #[doc(hidden)]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>>;

        #[doc(hidden)]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE>;
    }

    // ==== impls ====

    impl<VE: ValueEncoding> Sealed<VE> for MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>> {
            map.headers
                .iter()
                .find(|(k, _)| k == self.inner)
                .map(|(_, v)| MetadataValue::unchecked_from_header_value_ref(v))
        }

        #[doc(hidden)]
        #[inline]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE> {
            GetAll {
                map,
                key: Some(self),
            }
        }

        #[doc(hidden)]
        #[inline]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>> {
            let mut ret = None;

            map.headers.retain(|(k, v)| {
                if k == self.inner {
                    if ret.is_none() {
                        ret = Some(MetadataValue::unchecked_from_header_value(v.clone()));
                    }

                    false
                } else {
                    true
                }
            });
            ret
        }

        #[doc(hidden)]
        #[inline]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE> {
            let len = map.headers.len();
            let mut i = 0;
            let mut tail = len;

            while i < tail {
                if map.headers[i].0 == self.inner {
                    tail -= 1;
                    map.headers.swap(i, tail);
                } else {
                    i += 1;
                }
            }

            ValueDrain {
                inner: map.headers.drain(tail..),
                _phantom: PhantomData,
            }
        }
    }

    impl<VE: ValueEncoding> AsMetadataKey<VE> for MetadataKey<VE> {}

    impl<VE: ValueEncoding> Sealed<VE> for &MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>> {
            map.headers
                .iter()
                .find(|(k, _)| k == self.inner)
                .map(|(_, v)| MetadataValue::unchecked_from_header_value_ref(v))
        }

        #[doc(hidden)]
        #[inline]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE> {
            GetAll {
                map,
                key: Some(self.clone()),
            }
        }

        #[doc(hidden)]
        #[inline]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>> {
            let mut ret = None;

            map.headers.retain(|(k, v)| {
                if k == self.inner {
                    if ret.is_none() {
                        ret = Some(MetadataValue::unchecked_from_header_value(v.clone()));
                    }

                    false
                } else {
                    true
                }
            });
            ret
        }

        #[doc(hidden)]
        #[inline]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE> {
            let mut keep_idx = 0;

            for i in 0..map.headers.len() {
                if map.headers[i].0 != self.inner {
                    map.headers.swap(keep_idx, i);
                    keep_idx += 1;
                }
            }

            // Drain everything from `keep_idx` to the end.
            // These are the elements that matched the target key.
            ValueDrain {
                inner: map.headers.drain(keep_idx..),
                _phantom: PhantomData,
            }
        }
    }

    impl<VE: ValueEncoding> AsMetadataKey<VE> for &MetadataKey<VE> {}

    impl<VE: ValueEncoding> Sealed<VE> for &str {
        #[doc(hidden)]
        #[inline]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>> {
            if !VE::is_valid_key(self) {
                return None;
            }
            map.headers
                .iter()
                .find(|(k, _)| k == self)
                .map(|(_, v)| MetadataValue::unchecked_from_header_value_ref(v))
        }

        #[doc(hidden)]
        #[inline]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE> {
            let key = if VE::is_valid_key(self) {
                Some(MetadataKey::<VE>::from_bytes(self.as_bytes()).unwrap())
            } else {
                None
            };
            GetAll { map, key }
        }

        #[doc(hidden)]
        #[inline]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>> {
            if !VE::is_valid_key(self) {
                return None;
            }

            let mut ret = None;

            map.headers.retain(|(k, v)| {
                if k == self {
                    if ret.is_none() {
                        ret = Some(MetadataValue::unchecked_from_header_value(v.clone()));
                    }

                    false
                } else {
                    true
                }
            });
            ret
        }

        #[doc(hidden)]
        #[inline]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE> {
            if !VE::is_valid_key(self) {
                return ValueDrain {
                    inner: map.headers.drain(map.headers.len()..),
                    _phantom: PhantomData,
                };
            }

            let mut keep_idx = 0;

            for i in 0..map.headers.len() {
                if map.headers[i].0 != self {
                    map.headers.swap(keep_idx, i);
                    keep_idx += 1;
                }
            }

            // Drain everything from `keep_idx` to the end.
            // These are the elements that matched the target key.
            ValueDrain {
                inner: map.headers.drain(keep_idx..),
                _phantom: PhantomData,
            }
        }
    }

    impl<VE: ValueEncoding> AsMetadataKey<VE> for &str {}

    impl<VE: ValueEncoding> Sealed<VE> for String {
        #[doc(hidden)]
        #[inline]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>> {
            Sealed::<VE>::get(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE> {
            Sealed::<VE>::get_all(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>> {
            Sealed::<VE>::remove(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE> {
            Sealed::<VE>::remove_all(self.as_str(), map)
        }
    }

    impl<VE: ValueEncoding> AsMetadataKey<VE> for String {}

    impl<VE: ValueEncoding> Sealed<VE> for &String {
        #[doc(hidden)]
        #[inline]
        fn get(self, map: &MetadataMap) -> Option<&MetadataValue<VE>> {
            Sealed::<VE>::get(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn get_all(self, map: &MetadataMap) -> GetAll<'_, VE> {
            Sealed::<VE>::get_all(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn remove(self, map: &mut MetadataMap) -> Option<MetadataValue<VE>> {
            Sealed::<VE>::remove(self.as_str(), map)
        }

        #[doc(hidden)]
        #[inline]
        fn remove_all(self, map: &mut MetadataMap) -> ValueDrain<'_, VE> {
            Sealed::<VE>::remove_all(self.as_str(), map)
        }
    }

    impl<VE: ValueEncoding> AsMetadataKey<VE> for &String {}
}

mod as_encoding_agnostic_metadata_key {
    use super::MetadataMap;
    use super::ValueEncoding;
    use crate::metadata::key::MetadataKey;

    /// A marker trait used to identify values that can be used as search keys
    /// to a `MetadataMap`, for operations that don't expose the actual value.
    pub trait AsEncodingAgnosticMetadataKey: Sealed {}

    // All methods are on this pub(super) trait, instead of
    // `AsEncodingAgnosticMetadataKey`, so that they aren't publicly exposed to
    // the world.
    //
    // Being on the `AsEncodingAgnosticMetadataKey` trait would mean users could
    // call `"host".contains_key(&map)`.
    //
    // Ultimately, this allows us to adjust the signatures of these methods
    // without breaking any external crate.
    pub trait Sealed {
        #[doc(hidden)]
        fn contains_key(&self, map: &MetadataMap) -> bool;
    }

    // ==== impls ====

    impl<VE: ValueEncoding> Sealed for MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn contains_key(&self, map: &MetadataMap) -> bool {
            map.headers.iter().any(|(k, _)| k == self.inner)
        }
    }

    impl<VE: ValueEncoding> AsEncodingAgnosticMetadataKey for MetadataKey<VE> {}

    impl<VE: ValueEncoding> Sealed for &MetadataKey<VE> {
        #[doc(hidden)]
        #[inline]
        fn contains_key(&self, map: &MetadataMap) -> bool {
            map.headers.iter().any(|(k, _)| k == self.inner)
        }
    }

    impl<VE: ValueEncoding> AsEncodingAgnosticMetadataKey for &MetadataKey<VE> {}

    impl Sealed for &str {
        #[doc(hidden)]
        #[inline]
        fn contains_key(&self, map: &MetadataMap) -> bool {
            map.headers.iter().any(|(k, _)| k == *self)
        }
    }

    impl AsEncodingAgnosticMetadataKey for &str {}

    impl Sealed for String {
        #[doc(hidden)]
        #[inline]
        fn contains_key(&self, map: &MetadataMap) -> bool {
            map.headers.iter().any(|(k, _)| k == self.as_str())
        }
    }

    impl AsEncodingAgnosticMetadataKey for String {}

    impl Sealed for &String {
        #[doc(hidden)]
        #[inline]
        fn contains_key(&self, map: &MetadataMap) -> bool {
            map.headers.iter().any(|(k, _)| k == self.as_str())
        }
    }

    impl AsEncodingAgnosticMetadataKey for &String {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_headers_filters_invalid_headers() {
        let mut http_map = http::HeaderMap::new();

        // Valid ASCII
        http_map.insert("x-host", "example.com".parse().unwrap());
        // Valid Binary (decoded from base64)
        http_map.insert("trace-proto-bin", "SGVsbG8hIQ==".parse().unwrap());

        // Invalid gRPC key name (contains '!', which is valid in HTTP but not in gRPC metadata)
        http_map.insert(HeaderName::from_static("x-host!"), "val".parse().unwrap());

        // Invalid ASCII value (contains characters >= 127)
        // gRPC only allows visible ASCII [32-126].
        // Let's use a byte > 127 which is valid in HTTP HeaderValue but invalid
        // in gRPC MetadataValue<Ascii>.
        http_map.insert("x-invalid-ascii", HeaderValue::from_bytes(&[0xFA]).unwrap());

        // Invalid Binary value (not valid base64)
        http_map.insert("invalid-bin", "not-base64-!!!".parse().unwrap());

        let map = MetadataMap::from_headers(http_map);

        assert_eq!(map.len(), 2);
        assert_eq!(map.get("x-host").unwrap(), "example.com");
        assert_eq!(map.get_bin("trace-proto-bin").unwrap(), "Hello!!");

        assert!(!map.contains_key("x-host!"));
        assert!(!map.contains_key("x-invalid-ascii"));
        assert!(!map.contains_key("invalid-bin"));
    }

    #[test]
    fn test_iter_categorizes_ascii_entries() {
        let mut map = MetadataMap::new();

        map.insert("x-word", "hello".parse().unwrap());
        map.append_bin("x-word-bin", MetadataValue::from_bytes(b"goodbye"));
        map.insert_bin("x-number-bin", MetadataValue::from_bytes(b"123"));

        let mut found_x_word = false;
        for key_and_value in map.iter() {
            if let KeyAndValueRef::Ascii(key, _value) = key_and_value {
                if key.as_str() == "x-word" {
                    found_x_word = true;
                } else {
                    panic!("Unexpected key");
                }
            }
        }
        assert!(found_x_word);
    }

    #[test]
    fn test_iter_categorizes_binary_entries() {
        let mut map = MetadataMap::new();

        map.insert("x-word", "hello".parse().unwrap());
        map.append_bin("x-word-bin", MetadataValue::from_bytes(b"goodbye"));

        let mut found_x_word_bin = false;
        for key_and_value in map.iter() {
            if let KeyAndValueRef::Binary(key, _value) = key_and_value {
                if key.as_str() == "x-word-bin" {
                    found_x_word_bin = true;
                } else {
                    panic!("Unexpected key");
                }
            }
        }
        assert!(found_x_word_bin);
    }

    #[test]
    fn test_remove_all_preserves_other_keys_order() {
        let mut map = MetadataMap::new();
        map.append("a", "1".parse().unwrap());
        map.append("b", "2".parse().unwrap());
        map.append("a", "3".parse().unwrap());
        map.append("b", "4".parse().unwrap());

        map.remove_all("a");

        let keys: Vec<_> = map
            .iter()
            .map(|kv| match kv {
                KeyAndValueRef::Ascii(_, v) => v.to_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(keys, vec!["2", "4"]);
    }

    #[test]
    fn test_remove_all_bin() {
        let mut map = MetadataMap::new();
        map.insert_bin(
            "trace-proto-bin",
            MetadataValue::from_bytes(b"[binary data]"),
        );
        map.append_bin(
            "trace-proto-bin",
            MetadataValue::from_bytes(b"[binary data 2]"),
        );
        map.insert("x-host", "example.com".parse().unwrap());

        let mut bin_entries: Vec<_> = map.remove_all_bin("trace-proto-bin").collect();
        assert_eq!(2, bin_entries.len());
        bin_entries.sort();
        assert!(bin_entries.iter().any(|v| v == &b"[binary data]"[..]));
        assert!(bin_entries.iter().any(|v| v == &b"[binary data 2]"[..]));

        assert!(map.get_bin("trace-proto-bin").is_none());
        assert!(map.get("x-host").is_some());
    }

    #[test]
    fn test_retain() {
        let mut map = MetadataMap::new();
        map.insert("x-host", "hello".parse().unwrap());
        map.insert("x-number", "123".parse().unwrap());
        map.insert_bin("trace-proto-bin", MetadataValue::from_bytes(b"world"));

        map.retain(|entry| match entry {
            KeyAndValueRef::Ascii(key, _) => key == "x-host",
            _ => false,
        });

        assert_eq!(map.len(), 1);
        assert!(map.contains_key("x-host"));
        assert!(!map.contains_key("x-number"));
        assert!(!map.contains_key("trace-proto-bin"));
    }

    #[allow(dead_code)]
    fn value_drain_is_send_sync() {
        fn is_send_sync<T: Send + Sync>() {}

        is_send_sync::<Iter<'_>>();

        is_send_sync::<ValueDrain<'_, Ascii>>();
        is_send_sync::<ValueDrain<'_, Binary>>();
    }
}

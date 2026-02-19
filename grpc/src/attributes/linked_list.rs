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

use std::collections::BTreeSet;
use std::sync::Arc;

/// A node in the persistent linked list.
///
/// Each node represents an operation (insertion or deletion) and points to the
/// previous state of the list.
#[derive(Clone, Debug)]
enum Node<K, V> {
    /// Represents an insertion of a key-value pair.
    Entry {
        key: K,
        value: V,
        next: Option<Arc<Node<K, V>>>,
    },
    /// Represents a deletion of a key.
    Deletion {
        key: K,
        next: Option<Arc<Node<K, V>>>,
    },
}

/// A persistent linked list that behaves like a map.
///
/// This list is persistent, meaning that modifying it (adding or removing items)
/// returns a new version of the list, while preserving the old version. It uses
/// structural sharing to minimize memory usage.
///
/// The list supports shadowing: adding a key that already exists will effectively
/// update the value for that key in the new version of the list.
///
/// Deletions are handled by adding a "deletion marker" node to the front of the
/// list, which hides the key during iteration.
///
/// # Warning
///
/// This list is intended to store a small number of values (few hundreds) and
/// is optimized for memory usage. It is **not** optimized for query speed.
#[derive(Debug)]
pub struct LinkedList<K, V> {
    head: Option<Arc<Node<K, V>>>,
}

impl<K, V> Clone for LinkedList<K, V> {
    fn clone(&self) -> Self {
        Self {
            head: self.head.clone(),
        }
    }
}

impl<K, V> Default for LinkedList<K, V> {
    fn default() -> Self {
        Self { head: None }
    }
}

impl<K, V> LinkedList<K, V> {
    /// Creates a new, empty persistent linked list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a key-value pair to the front of the list.
    ///
    /// If the key already exists in the list, this new entry will shadow the
    /// old one, effectively updating the value.
    pub fn add(&self, key: K, value: V) -> Self {
        LinkedList {
            head: Some(Arc::new(Node::Entry {
                key,
                value,
                next: self.head.clone(),
            })),
        }
    }

    /// Removes a key from the list.
    ///
    /// This operation adds a deletion marker to the front of the list, which will
    /// prevent the key from being yielded during iteration.
    pub fn remove(&self, key: K) -> Self {
        LinkedList {
            head: Some(Arc::new(Node::Deletion {
                key,
                next: self.head.clone(),
            })),
        }
    }
}

impl<K: Eq, V> LinkedList<K, V> {
    /// Gets the value associated with the given key.
    ///
    /// This method iterates through the list from the front to find the most recent
    /// entry for the key. If a deletion marker is encountered for the key, `None`
    /// is returned.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up.
    ///
    /// # Returns
    ///
    /// The value associated with the key, or `None` if the key is not present or
    /// has been removed.
    pub fn get(&self, key: &K) -> Option<&V> {
        let mut current = self.head.as_ref();
        while let Some(node) = current {
            match &**node {
                Node::Entry {
                    key: k,
                    value,
                    next,
                } => {
                    if k == key {
                        return Some(value);
                    }
                    current = next.as_ref();
                }
                Node::Deletion { key: k, next } => {
                    if k == key {
                        return None;
                    }
                    current = next.as_ref();
                }
            }
        }
        None
    }
}

impl<K: Ord, V> LinkedList<K, V> {
    /// Returns an iterator over the key-value pairs in the list.
    ///
    /// The iterator yields unique keys. If a key has been added multiple times,
    /// only the most recent value is returned. Keys that have been removed are
    /// skipped.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            current: self.head.as_ref(),
            seen: BTreeSet::new(),
        }
    }
}

/// An iterator over the items of a `LinkedList`.
pub struct Iter<'a, K, V> {
    current: Option<&'a Arc<Node<K, V>>>,
    seen: BTreeSet<&'a K>,
}

impl<'a, K: Ord, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let node = self.current?;
            match &**node {
                Node::Entry { key, value, next } => {
                    self.current = next.as_ref();
                    if self.seen.insert(key) {
                        return Some((key, value));
                    }
                }
                Node::Deletion { key, next } => {
                    self.seen.insert(key);
                    self.current = next.as_ref();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_iter() {
        let l = LinkedList::new().add(1, "a").add(2, "b").add(3, "c");
        let v: Vec<_> = l.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(v, vec![(3, "c"), (2, "b"), (1, "a")]);
    }

    #[test]
    fn test_remove() {
        let l = LinkedList::new().add(1, "a").add(2, "b").add(3, "c");
        let l2 = l.remove(2);
        let v: Vec<_> = l2.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(v, vec![(3, "c"), (1, "a")]);
    }

    #[test]
    fn test_persistence() {
        let l1 = LinkedList::new().add(1, "a");
        let l2 = l1.add(2, "b");

        // l1 should be unchanged
        let v1: Vec<_> = l1.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(v1, vec![(1, "a")]);

        // l2 should have both
        let v2: Vec<_> = l2.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(v2, vec![(2, "b"), (1, "a")]);
    }

    #[test]
    fn test_reinsertion() {
        let l = LinkedList::new().add(1, "a");
        let l = l.remove(1);
        let l = l.add(1, "b");

        let v: Vec<_> = l.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(v, vec![(1, "b")]);
    }

    #[test]
    fn test_multiple_removals() {
        let l = LinkedList::new().add(1, "a").add(1, "b"); // Second add shadows first
        let l = l.remove(1);
        let v: Vec<_> = l.iter().map(|(k, v)| (*k, *v)).collect();
        assert!(v.is_empty());
    }

    #[test]
    fn test_shadowing() {
        let l = LinkedList::new().add(1, "a").add(1, "b");
        let v: Vec<_> = l.iter().map(|(k, v)| (*k, *v)).collect();
        // Should return the most recently added value for key 1
        assert_eq!(v, vec![(1, "b")]);
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LinkedList<i32, i32>>();
    }
}

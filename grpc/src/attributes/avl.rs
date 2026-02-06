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

use std::cmp::{max, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct Node<K, V> {
    key: K,
    value: V,
    left: Option<Arc<Node<K, V>>>,
    right: Option<Arc<Node<K, V>>>,
    height: usize,
}

impl<K, V> Node<K, V> {
    fn new(
        key: K,
        value: V,
        left: Option<Arc<Node<K, V>>>,
        right: Option<Arc<Node<K, V>>>,
    ) -> Self {
        let lh = left.as_ref().map_or(0, |n| n.height);
        let rh = right.as_ref().map_or(0, |n| n.height);
        Node {
            key,
            value,
            left,
            right,
            height: 1 + max(lh, rh),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Avl<K, V> {
    root: Option<Arc<Node<K, V>>>,
}

impl<K, V> Default for Avl<K, V> {
    fn default() -> Self {
        Self { root: None }
    }
}

impl<K, V> Avl<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn height(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.height)
    }

    pub fn iter(&self) -> Iter<'_, K, V> {
        let mut iter = Iter { stack: Vec::new() };
        iter.push_left(&self.root);
        iter
    }
}

impl<K: Ord + PartialEq, V: PartialEq> PartialEq for Avl<K, V> {
    fn eq(&self, other: &Self) -> bool {
        if let (Some(r1), Some(r2)) = (&self.root, &other.root) {
            if Arc::ptr_eq(r1, r2) {
                return true;
            }
        } else if self.root.is_none() && other.root.is_none() {
            return true;
        }
        self.iter().eq(other.iter())
    }
}

impl<K: Ord + Eq, V: Eq> Eq for Avl<K, V> {}

impl<K: Ord, V: PartialOrd> PartialOrd for Avl<K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if let (Some(r1), Some(r2)) = (&self.root, &other.root) {
            if Arc::ptr_eq(r1, r2) {
                return Some(Ordering::Equal);
            }
        } else if self.root.is_none() && other.root.is_none() {
            return Some(Ordering::Equal);
        }
        self.iter().partial_cmp(other.iter())
    }
}

impl<K: Ord, V: Ord> Ord for Avl<K, V> {
    fn cmp(&self, other: &Self) -> Ordering {
        if let (Some(r1), Some(r2)) = (&self.root, &other.root) {
            if Arc::ptr_eq(r1, r2) {
                return Ordering::Equal;
            }
        } else if self.root.is_none() && other.root.is_none() {
            return Ordering::Equal;
        }
        self.iter().cmp(other.iter())
    }
}

impl<K: Ord, V> Avl<K, V> {
    pub fn get(&self, key: &K) -> Option<&V> {
        let mut current = self.root.as_ref();
        while let Some(node) = current {
            match key.cmp(&node.key) {
                Ordering::Less => current = node.left.as_ref(),
                Ordering::Greater => current = node.right.as_ref(),
                Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }
}

impl<K: Clone + Ord, V: Clone> Avl<K, V> {
    pub fn add(&self, key: K, value: V) -> Self {
        Avl {
            root: Self::add_node(self.root.as_ref(), key, value),
        }
    }

    fn add_node(node: Option<&Arc<Node<K, V>>>, key: K, value: V) -> Option<Arc<Node<K, V>>> {
        let Some(node) = node else {
            return Some(Arc::new(Node::new(key, value, None, None)));
        };

        match key.cmp(&node.key) {
            Ordering::Less => {
                let new_left = Self::add_node(node.left.as_ref(), key, value);
                Self::rebalance(
                    node.key.clone(),
                    node.value.clone(),
                    new_left,
                    node.right.clone(),
                )
            }
            Ordering::Greater => {
                let new_right = Self::add_node(node.right.as_ref(), key, value);
                Self::rebalance(
                    node.key.clone(),
                    node.value.clone(),
                    node.left.clone(),
                    new_right,
                )
            }
            Ordering::Equal => {
                // Key exists, replace value
                Some(Arc::new(Node::new(
                    key,
                    value,
                    node.left.clone(),
                    node.right.clone(),
                )))
            }
        }
    }

    pub fn remove(&self, key: &K) -> Self {
        Avl {
            root: Self::remove_node(self.root.as_ref(), key),
        }
    }

    fn remove_node(node: Option<&Arc<Node<K, V>>>, key: &K) -> Option<Arc<Node<K, V>>> {
        let node = node?;

        match key.cmp(&node.key) {
            Ordering::Less => {
                let new_left = Self::remove_node(node.left.as_ref(), key);
                Self::rebalance(
                    node.key.clone(),
                    node.value.clone(),
                    new_left,
                    node.right.clone(),
                )
            }
            Ordering::Greater => {
                let new_right = Self::remove_node(node.right.as_ref(), key);
                Self::rebalance(
                    node.key.clone(),
                    node.value.clone(),
                    node.left.clone(),
                    new_right,
                )
            }
            Ordering::Equal => {
                if node.left.is_none() {
                    return node.right.clone();
                }
                if node.right.is_none() {
                    return node.left.clone();
                }

                let left_height = node.left.as_ref().map_or(0, |n| n.height);
                let right_height = node.right.as_ref().map_or(0, |n| n.height);

                if left_height < right_height {
                    let min_right = Self::min_node(node.right.as_ref().unwrap());
                    let new_right = Self::remove_node(node.right.as_ref(), &min_right.key);
                    Self::rebalance(
                        min_right.key.clone(),
                        min_right.value.clone(),
                        node.left.clone(),
                        new_right,
                    )
                } else {
                    let max_left = Self::max_node(node.left.as_ref().unwrap());
                    let new_left = Self::remove_node(node.left.as_ref(), &max_left.key);
                    Self::rebalance(
                        max_left.key.clone(),
                        max_left.value.clone(),
                        new_left,
                        node.right.clone(),
                    )
                }
            }
        }
    }

    fn min_node(node: &Arc<Node<K, V>>) -> &Arc<Node<K, V>> {
        if let Some(left) = &node.left {
            Self::min_node(left)
        } else {
            node
        }
    }

    fn max_node(node: &Arc<Node<K, V>>) -> &Arc<Node<K, V>> {
        if let Some(right) = &node.right {
            Self::max_node(right)
        } else {
            node
        }
    }

    fn height_of(node: &Option<Arc<Node<K, V>>>) -> usize {
        node.as_ref().map_or(0, |n| n.height)
    }

    fn rebalance(
        key: K,
        value: V,
        left: Option<Arc<Node<K, V>>>,
        right: Option<Arc<Node<K, V>>>,
    ) -> Option<Arc<Node<K, V>>> {
        let lh = Self::height_of(&left) as isize;
        let rh = Self::height_of(&right) as isize;
        let balance = lh - rh;

        if balance == 2 {
            let left_node = left.as_ref().unwrap();
            let llh = Self::height_of(&left_node.left) as isize;
            let lrh = Self::height_of(&left_node.right) as isize;
            if llh - lrh == -1 {
                Self::rotate_left_right(key, value, left.unwrap(), right)
            } else {
                Self::rotate_right(key, value, left.unwrap(), right)
            }
        } else if balance == -2 {
            let right_node = right.as_ref().unwrap();
            let rlh = Self::height_of(&right_node.left) as isize;
            let rrh = Self::height_of(&right_node.right) as isize;
            if rlh - rrh == 1 {
                Self::rotate_right_left(key, value, left, right.unwrap())
            } else {
                Self::rotate_left(key, value, left, right.unwrap())
            }
        } else {
            Some(Arc::new(Node::new(key, value, left, right)))
        }
    }

    fn rotate_left(
        key: K,
        value: V,
        left: Option<Arc<Node<K, V>>>,
        right: Arc<Node<K, V>>,
    ) -> Option<Arc<Node<K, V>>> {
        // Parent becomes left child of right
        let new_left = Arc::new(Node::new(key, value, left, right.left.clone()));
        Some(Arc::new(Node::new(
            right.key.clone(),
            right.value.clone(),
            Some(new_left),
            right.right.clone(),
        )))
    }

    fn rotate_right(
        key: K,
        value: V,
        left: Arc<Node<K, V>>,
        right: Option<Arc<Node<K, V>>>,
    ) -> Option<Arc<Node<K, V>>> {
        // Parent becomes right child of left
        let new_right = Arc::new(Node::new(key, value, left.right.clone(), right));
        Some(Arc::new(Node::new(
            left.key.clone(),
            left.value.clone(),
            left.left.clone(),
            Some(new_right),
        )))
    }

    fn rotate_left_right(
        key: K,
        value: V,
        left: Arc<Node<K, V>>,
        right: Option<Arc<Node<K, V>>>,
    ) -> Option<Arc<Node<K, V>>> {
        // Rotate left on left child, then rotate right on parent
        let left_right = left.right.as_ref().unwrap();

        // New left for the final root
        let new_left = Arc::new(Node::new(
            left.key.clone(),
            left.value.clone(),
            left.left.clone(),
            left_right.left.clone(),
        ));

        // New right for the final root
        let new_right = Arc::new(Node::new(key, value, left_right.right.clone(), right));

        Some(Arc::new(Node::new(
            left_right.key.clone(),
            left_right.value.clone(),
            Some(new_left),
            Some(new_right),
        )))
    }

    fn rotate_right_left(
        key: K,
        value: V,
        left: Option<Arc<Node<K, V>>>,
        right: Arc<Node<K, V>>,
    ) -> Option<Arc<Node<K, V>>> {
        // Rotate right on right child, then rotate left on parent
        let right_left = right.left.as_ref().unwrap();

        // New left for the final root
        let new_left = Arc::new(Node::new(key, value, left, right_left.left.clone()));

        // New right for the final root
        let new_right = Arc::new(Node::new(
            right.key.clone(),
            right.value.clone(),
            right_left.right.clone(),
            right.right.clone(),
        ));

        Some(Arc::new(Node::new(
            right_left.key.clone(),
            right_left.value.clone(),
            Some(new_left),
            Some(new_right),
        )))
    }
}

pub struct Iter<'a, K, V> {
    stack: Vec<&'a Arc<Node<K, V>>>,
}

impl<'a, K, V> Iter<'a, K, V> {
    fn push_left(&mut self, mut node: &'a Option<Arc<Node<K, V>>>) {
        while let Some(n) = node {
            self.stack.push(n);
            node = &n.left;
        }
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.push_left(&node.right);
        Some((&node.key, &node.value))
    }
}

impl<'a, K, V> IntoIterator for &'a Avl<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let t = Avl::new();
        let t = t.add(1, "one");
        let t = t.add(2, "two");
        let t = t.add(3, "three");

        assert_eq!(t.get(&1), Some(&"one"));
        assert_eq!(t.get(&2), Some(&"two"));
        assert_eq!(t.get(&3), Some(&"three"));
        assert_eq!(t.get(&4), None);
    }

    #[test]
    fn test_overwrite() {
        let t = Avl::new();
        let t = t.add(1, "one");
        let t = t.add(1, "ONE");
        assert_eq!(t.get(&1), Some(&"ONE"));
    }

    #[test]
    fn test_persistence() {
        let t1 = Avl::new().add(1, "one");
        let t2 = t1.add(2, "two");

        assert_eq!(t1.get(&1), Some(&"one"));
        assert_eq!(t1.get(&2), None);

        assert_eq!(t2.get(&1), Some(&"one"));
        assert_eq!(t2.get(&2), Some(&"two"));
    }

    #[test]
    fn test_remove() {
        let t = Avl::new().add(1, 1).add(2, 2).add(3, 3);
        let t2 = t.remove(&2);

        assert_eq!(t.get(&2), Some(&2));
        assert_eq!(t2.get(&2), None);
        assert_eq!(t2.get(&1), Some(&1));
        assert_eq!(t2.get(&3), Some(&3));
    }

    #[test]
    fn test_iter() {
        let t = Avl::new().add(3, 3).add(1, 1).add(2, 2).add(4, 4);
        let pairs: Vec<_> = t.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(pairs, vec![(1, 1), (2, 2), (3, 3), (4, 4)]);
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Avl<i32, i32>>();
    }

    #[test]
    fn test_empty_tree_behavior() {
        let t: Avl<i32, i32> = Avl::new();
        assert!(t.is_empty());
        assert_eq!(t.height(), 0);
        assert_eq!(t.get(&1), None);

        let t2 = t.remove(&1);
        assert!(t2.is_empty());

        assert_eq!(t.iter().next(), None);
    }

    #[test]
    fn test_rotations() {
        // Left Rotation (Right Heavy)
        let t = Avl::new().add(1, 1).add(2, 2).add(3, 3);
        assert_eq!(t.height(), 2);
        assert_eq!(t.root.as_ref().unwrap().key, 2);

        // Right Rotation (Left Heavy)
        let t = Avl::new().add(3, 3).add(2, 2).add(1, 1);
        assert_eq!(t.height(), 2);
        assert_eq!(t.root.as_ref().unwrap().key, 2);

        // Left-Right Rotation
        let t = Avl::new().add(3, 3).add(1, 1).add(2, 2);
        assert_eq!(t.height(), 2);
        assert_eq!(t.root.as_ref().unwrap().key, 2);

        // Right-Left Rotation
        let t = Avl::new().add(1, 1).add(3, 3).add(2, 2);
        assert_eq!(t.height(), 2);
        assert_eq!(t.root.as_ref().unwrap().key, 2);
    }

    #[test]
    fn test_remove_root_with_children() {
        // Tree:
        //      4
        //    /   \
        //   2     6
        //  / \   / \
        // 1   3 5   7
        let t = Avl::new()
            .add(4, 4)
            .add(2, 2)
            .add(6, 6)
            .add(1, 1)
            .add(3, 3)
            .add(5, 5)
            .add(7, 7);

        assert_eq!(t.height(), 3);

        // Remove root 4.
        // Left height = 2, Right height = 2.
        // Implementation logic: if left_height < right_height { min_right } else { max_left }
        // 2 < 2 is false, so it chooses max_left (3).
        let t2 = t.remove(&4);

        assert_eq!(t2.root.as_ref().unwrap().key, 3);
        assert_eq!(t2.get(&4), None);
        // Verify structure integrity
        assert_eq!(t2.get(&1), Some(&1));
        assert_eq!(t2.get(&2), Some(&2));
        assert_eq!(t2.get(&3), Some(&3));
        assert_eq!(t2.get(&5), Some(&5));
        assert_eq!(t2.get(&6), Some(&6));
        assert_eq!(t2.get(&7), Some(&7));
    }

    #[test]
    fn test_rebalance_after_remove() {
        // Construct a tree that needs rebalancing after removal
        //      4
        //     / \
        //    2   5
        //   / \
        //  1   3
        let t = Avl::new().add(4, 4).add(2, 2).add(5, 5).add(1, 1).add(3, 3);

        // Remove 5. Tree becomes left heavy at 4.
        //      4
        //     /
        //    2
        //   / \
        //  1   3
        // Should rotate right around 4. New root 2.
        let t2 = t.remove(&5);

        assert_eq!(t2.root.as_ref().unwrap().key, 2);
        assert_eq!(t2.height(), 3); // Root 2 -> Right 4 -> Left 3
    }

    #[test]
    fn test_eq_ord() {
        let t1 = Avl::new().add(1, 1).add(2, 2);
        let t2 = Avl::new().add(2, 2).add(1, 1); // Insert order differs, but set is same
        let t3 = Avl::new().add(1, 1);

        assert_eq!(t1, t2);
        assert!(t1 > t3);
        assert!(t3 < t1);

        let t4 = Avl::new().add(1, 2); // Different value
        assert_ne!(t1, t4);
    }
}

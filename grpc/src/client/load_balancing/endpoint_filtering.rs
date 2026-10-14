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

use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;

use crate::client::name_resolution::Endpoint;

#[derive(Clone)]
struct ArcSlice {
    data: Arc<[String]>,
    start: usize,
}

impl ArcSlice {
    fn new(vec: Vec<String>) -> Self {
        let len = vec.len();
        Self {
            data: Arc::from(vec),
            start: 0,
        }
    }

    fn pop_front(&self) -> Self {
        assert!(self.data.len() - self.start > 0);
        Self {
            data: Arc::clone(&self.data),
            start: self.start + 1,
        }
    }
}

impl Deref for ArcSlice {
    type Target = [String];
    fn deref(&self) -> &[String] {
        &self.data[self.start..]
    }
}

impl Debug for ArcSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&**self, f)
    }
}

impl PartialEq for ArcSlice {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: if they point to the exact same Arc allocation and have
        // identical bounds.
        if Arc::ptr_eq(&self.data, &other.data) && self.start == other.start {
            return true;
        }

        // Compare the actual slices element-by-element by value.
        **self == **other
    }
}

impl Eq for ArcSlice {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HierarchicalPath {
    parts: ArcSlice,
}

impl HierarchicalPath {
    fn new(parts: Vec<String>) -> Self {
        Self {
            parts: ArcSlice::new(parts),
        }
    }

    fn new_with_arc_slice(parts: ArcSlice) -> Self {
        Self { parts }
    }

    /// Returns the hierarchical path of endpoint.
    fn from_endpoint(endpoint: &Endpoint) -> Option<&Self> {
        endpoint.attributes.get::<Self>()
    }

    fn set_in_endpoint(self, mut endpoint: Endpoint) -> Endpoint {
        endpoint.attributes = endpoint.attributes.add(self);
        endpoint
    }
}

/// Overrides the hierarchical path in endpoint with path.
pub fn set_path_in_endpoint(endpoint: Endpoint, path: Vec<String>) -> Endpoint {
    HierarchicalPath::new(path).set_in_endpoint(endpoint)
}

/// Splits a slice of endpoints into groups based on the first hierarchy path.
/// The first hierarchy path will be removed from the result.
///
/// If hierarchical path is not set, or has no path in it, the endpoint is
/// dropped.
pub fn group_by_path(endpoints: Vec<Endpoint>) -> HashMap<String, Vec<Endpoint>> {
    let mut ret = HashMap::new();
    for endpoint in endpoints {
        if let Some(path) = HierarchicalPath::from_endpoint(&endpoint) {
            if path.parts.is_empty() {
                continue;
            }
            let cur_path = path.parts[0].clone();
            let new_path = HierarchicalPath::new_with_arc_slice(path.parts.pop_front());
            let new_endpoint = new_path.set_in_endpoint(endpoint);
            ret.entry(cur_path)
                .or_insert_with(Vec::new)
                .push(new_endpoint);
        }
    }
    ret
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::attributes::Attributes;
    use crate::byte_str::ByteStr;
    use crate::core::Address;

    fn test_endpoint(addr: &str) -> Endpoint {
        Endpoint {
            addresses: vec![Address {
                network_type: "tcp",
                address: ByteStr::from(addr.to_string()),
                attributes: Attributes::new(),
            }],
            attributes: Attributes::new(),
        }
    }

    #[test]
    fn test_from_endpoint() {
        let ep_not_set = test_endpoint("a");
        assert_eq!(HierarchicalPath::from_endpoint(&ep_not_set), None);

        let ep_set =
            set_path_in_endpoint(test_endpoint("a"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            HierarchicalPath::from_endpoint(&ep_set),
            Some(&HierarchicalPath::new(vec![
                "a".to_string(),
                "b".to_string()
            ]))
        );
    }

    #[test]
    fn test_set_in_endpoint() {
        // before is not set
        let ep = test_endpoint("a");
        let path = vec!["a".to_string(), "b".to_string()];
        let new_ep = set_path_in_endpoint(ep, path.clone());
        assert_eq!(
            HierarchicalPath::from_endpoint(&new_ep),
            Some(&HierarchicalPath::new(path.clone()))
        );

        // before is set
        let ep = set_path_in_endpoint(
            test_endpoint("a"),
            vec!["before".to_string(), "a".to_string(), "b".to_string()],
        );
        let path = vec!["a".to_string(), "b".to_string()];
        let new_ep = set_path_in_endpoint(ep, path.clone());
        assert_eq!(
            HierarchicalPath::from_endpoint(&new_ep),
            Some(&HierarchicalPath::new(path))
        );
    }

    #[test]
    fn test_group_with_hierarchy() {
        let eps = vec![
            set_path_in_endpoint(
                test_endpoint("a0"),
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            set_path_in_endpoint(test_endpoint("a1"), vec!["a".to_string(), "b".to_string()]),
            set_path_in_endpoint(
                test_endpoint("b0"),
                vec!["b".to_string(), "c".to_string(), "d".to_string()],
            ),
            set_path_in_endpoint(test_endpoint("b1"), vec!["b".to_string(), "c".to_string()]),
        ];

        // 1st group call: splits by first level ("a" and "b")
        let mut g1 = group_by_path(eps);
        assert_eq!(g1.len(), 2);

        let eps_a = g1.remove("a").unwrap();
        let eps_b = g1.remove("b").unwrap();

        assert_eq!(
            eps_a,
            vec![
                set_path_in_endpoint(test_endpoint("a0"), vec!["b".to_string(), "c".to_string()],),
                set_path_in_endpoint(test_endpoint("a1"), vec!["b".to_string()],),
            ]
        );
        assert_eq!(
            eps_b,
            vec![
                set_path_in_endpoint(test_endpoint("b0"), vec!["c".to_string(), "d".to_string()],),
                set_path_in_endpoint(test_endpoint("b1"), vec!["c".to_string()],),
            ]
        );

        // 2nd group call: splits by second level ("b" for group "a", "c" for
        // group "b")
        let mut g2_a = group_by_path(eps_a);
        let mut g2_b = group_by_path(eps_b);
        assert_eq!(g2_a.len(), 1);
        assert_eq!(g2_b.len(), 1);

        let eps_ab = g2_a.remove("b").unwrap();
        let eps_bc = g2_b.remove("c").unwrap();

        assert_eq!(
            eps_ab,
            vec![
                set_path_in_endpoint(test_endpoint("a0"), vec!["c".to_string()],),
                set_path_in_endpoint(test_endpoint("a1"), vec![]),
            ]
        );
        assert_eq!(
            eps_bc,
            vec![
                set_path_in_endpoint(test_endpoint("b0"), vec!["d".to_string()],),
                set_path_in_endpoint(test_endpoint("b1"), vec![]),
            ]
        );

        // 3rd group call: 2-level endpoints (a1, b1) have empty hierarchy and
        // are dropped. 3-level endpoints (a0, b0) have their hierarchy become
        // empty after 3 calls.
        let mut g3_a = group_by_path(eps_ab);
        let mut g3_b = group_by_path(eps_bc);
        assert_eq!(g3_a.len(), 1);
        assert_eq!(g3_b.len(), 1);

        let eps_abc = g3_a.remove("c").unwrap();
        let eps_bcd = g3_b.remove("d").unwrap();

        assert_eq!(
            eps_abc,
            vec![set_path_in_endpoint(test_endpoint("a0"), vec![],)]
        );
        assert_eq!(
            eps_bcd,
            vec![set_path_in_endpoint(test_endpoint("b0"), vec![],)]
        );

        // Calling group on endpoints that now have empty hierarchy returns an
        // empty map.
        assert!(group_by_path(eps_abc).is_empty());
        assert!(group_by_path(eps_bcd).is_empty());
    }
}

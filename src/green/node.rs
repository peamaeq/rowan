use std::{ffi::c_void, fmt, iter::FusedIterator, mem, slice};

use triomphe::{Arc, ThinArc};

use crate::{
    green::{GreenElement, GreenElementRef, SyntaxKind},
    utility_types::static_assert,
    GreenToken, NodeOrToken, TextRange, TextSize,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct GreenNodeHead {
    kind: SyntaxKind,
    text_len: TextSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GreenChild {
    Node { offset_in_parent: TextSize, node: GreenNode },
    Token { offset_in_parent: TextSize, token: GreenToken },
}

#[cfg(target_pointer_width = "64")]
static_assert!(mem::size_of::<GreenChild>() == mem::size_of::<usize>() * 2);

/// Internal node in the immutable tree.
/// It has other nodes and tokens as children.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GreenNode {
    data: ThinArc<GreenNodeHead, GreenChild>,
}

impl fmt::Debug for GreenNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GreenNode")
            .field("kind", &self.kind())
            .field("text_len", &self.text_len())
            .field("n_children", &self.children().len())
            .finish()
    }
}

impl GreenChild {
    fn as_ref(&self) -> GreenElementRef {
        match self {
            GreenChild::Node { node, .. } => NodeOrToken::Node(node),
            GreenChild::Token { token, .. } => NodeOrToken::Token(token),
        }
    }
    fn offset_in_parent(&self) -> TextSize {
        match self {
            GreenChild::Node { offset_in_parent, .. }
            | GreenChild::Token { offset_in_parent, .. } => *offset_in_parent,
        }
    }
    fn range_in_parent(&self) -> TextRange {
        let len = self.as_ref().text_len();
        TextRange::at(self.offset_in_parent(), len)
    }
}

impl GreenNode {
    /// Creates new Node.
    #[inline]
    pub fn new<I>(kind: SyntaxKind, children: I) -> GreenNode
    where
        I: IntoIterator<Item = GreenElement>,
        I::IntoIter: ExactSizeIterator,
    {
        let mut text_len: TextSize = 0.into();
        let children = children.into_iter().map(|el| {
            let offset_in_parent = text_len;
            text_len += el.text_len();
            match el {
                NodeOrToken::Node(node) => GreenChild::Node { offset_in_parent, node },
                NodeOrToken::Token(token) => GreenChild::Token { offset_in_parent, token },
            }
        });

        let data =
            ThinArc::from_header_and_iter(GreenNodeHead { kind, text_len: 0.into() }, children);

        // XXX: fixup `text_len` after construction, because we can't iterate
        // `children` twice.
        let data = {
            let mut data = Arc::from_thin(data);
            Arc::get_mut(&mut data).unwrap().header.header.text_len = text_len;
            Arc::into_thin(data)
        };

        GreenNode { data }
    }

    /// Kind of this node.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.data.header.header.kind
    }

    /// Returns the length of the text covered by this node.
    #[inline]
    pub fn text_len(&self) -> TextSize {
        self.data.header.header.text_len
    }

    /// Children of this node.
    #[inline]
    pub fn children(&self) -> Children<'_> {
        Children { inner: self.data.slice.iter() }
    }

    pub(crate) fn child_at_range(
        &self,
        range: TextRange,
    ) -> Option<(usize, TextSize, GreenElementRef<'_>)> {
        let idx = self
            .data
            .slice
            .binary_search_by(|it| {
                let child_range = it.range_in_parent();
                TextRange::ordering(child_range, range)
            })
            // XXX: this handles empty ranges
            .unwrap_or_else(|it| it.saturating_sub(1));
        let child =
            &self.data.slice.get(idx).filter(|it| it.range_in_parent().contains_range(range))?;
        Some((idx, child.offset_in_parent(), child.as_ref()))
    }

    pub fn ptr(&self) -> *const c_void {
        self.data.heap_ptr()
    }

    pub(crate) fn replace_child(&self, idx: usize, new_child: GreenElement) -> GreenNode {
        let mut replacement = Some(new_child);
        let children = self.children().enumerate().map(|(i, child)| {
            if i == idx {
                replacement.take().unwrap()
            } else {
                child.cloned()
            }
        });
        GreenNode::new(self.kind(), children)
    }
}

#[derive(Debug, Clone)]
pub struct Children<'a> {
    inner: slice::Iter<'a, GreenChild>,
}

// NB: forward everything stable that iter::Slice specializes as of Rust 1.39.0
impl ExactSizeIterator for Children<'_> {
    #[inline(always)]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<'a> Iterator for Children<'a> {
    type Item = GreenElementRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<GreenElementRef<'a>> {
        self.inner.next().map(GreenChild::as_ref)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    #[inline]
    fn count(self) -> usize
    where
        Self: Sized,
    {
        self.inner.count()
    }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.inner.nth(n).map(GreenChild::as_ref)
    }

    #[inline]
    fn last(mut self) -> Option<Self::Item>
    where
        Self: Sized,
    {
        self.next_back()
    }

    #[inline]
    fn fold<Acc, Fold>(mut self, init: Acc, mut f: Fold) -> Acc
    where
        Fold: FnMut(Acc, Self::Item) -> Acc,
    {
        let mut accum = init;
        while let Some(x) = self.next() {
            accum = f(accum, x);
        }
        accum
    }
}

impl<'a> DoubleEndedIterator for Children<'a> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(GreenChild::as_ref)
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        self.inner.nth_back(n).map(GreenChild::as_ref)
    }

    #[inline]
    fn rfold<Acc, Fold>(mut self, init: Acc, mut f: Fold) -> Acc
    where
        Fold: FnMut(Acc, Self::Item) -> Acc,
    {
        let mut accum = init;
        while let Some(x) = self.next_back() {
            accum = f(accum, x);
        }
        accum
    }
}

impl FusedIterator for Children<'_> {}

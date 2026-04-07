//! Intrusive LIFO free-list.
//!
//! Free objects store a pointer to the next free object in their first bytes,
//! the same trick used by Linux's SLUB allocator.

use core::ptr::NonNull;

/// Single word stored at the start of a free object.
#[repr(C)]
pub(crate) struct FreeNode {
    pub next: Option<NonNull<FreeNode>>,
}

/// Minimum object size: every slab object must be large enough to hold
/// a free-list pointer.
///
/// On 64-bit targets this is **8 bytes**.
///
/// # Examples
///
/// ```
/// use slab_allocator::freelist::MIN_OBJ_SIZE;
/// assert!(MIN_OBJ_SIZE >= 8);
/// ```
pub const MIN_OBJ_SIZE: usize = core::mem::size_of::<FreeNode>();

/// Intrusive LIFO stack of free objects.
pub struct FreeList {
    head: Option<NonNull<FreeNode>>,
}

impl FreeList {
    /// Create an empty free-list.
    #[inline]
    pub const fn new() -> Self {
        Self { head: None }
    }

    /// Returns `true` if no free objects are available.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Push `ptr` onto the free-list.
    ///
    /// # Safety
    /// - `ptr` must point to at least `MIN_OBJ_SIZE` bytes of writable memory.
    /// - `ptr` must be aligned to `align_of::<FreeNode>()`.
    /// - The memory must not be accessed by anyone else until the next `pop`.
    #[inline]
    pub unsafe fn push(&mut self, ptr: NonNull<u8>) {
        let node = ptr.cast::<FreeNode>();
        // SAFETY: caller guarantees alignment and exclusivity.
        unsafe { node.as_ptr().write(FreeNode { next: self.head }) };
        self.head = Some(node);
    }

    /// Pop one object from the free-list. Returns `None` when empty.
    ///
    /// # Safety
    /// All pointers in the list must be valid (written by `push` and still live).
    #[inline]
    pub unsafe fn pop(&mut self) -> Option<NonNull<u8>> {
        let node = self.head?;
        // SAFETY: node was written by a previous push and is still live.
        let next = unsafe { node.as_ref().next };
        self.head = next;
        Some(node.cast::<u8>())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn make_buf(n: usize) -> std::vec::Vec<u64> {
        std::vec![0u64; n]
    }

    #[test]
    fn empty_list_pop_returns_none() {
        let mut fl = FreeList::new();
        assert!(unsafe { fl.pop() }.is_none());
    }

    #[test]
    fn push_pop_single() {
        let mut buf = make_buf(1);
        let ptr = NonNull::new(buf.as_mut_ptr().cast::<u8>()).unwrap();
        let mut fl = FreeList::new();
        unsafe { fl.push(ptr) };
        let got = unsafe { fl.pop() }.expect("should have one element");
        assert_eq!(got, ptr);
        assert!(unsafe { fl.pop() }.is_none());
    }

    #[test]
    fn lifo_ordering() {
        let mut buf = make_buf(3);
        let p0 = NonNull::new(buf.as_mut_ptr().cast::<u8>()).unwrap();
        let p1 = NonNull::new(unsafe { buf.as_mut_ptr().add(1) }.cast::<u8>()).unwrap();
        let p2 = NonNull::new(unsafe { buf.as_mut_ptr().add(2) }.cast::<u8>()).unwrap();
        let mut fl = FreeList::new();
        unsafe {
            fl.push(p0);
            fl.push(p1);
            fl.push(p2);
        }
        assert_eq!(unsafe { fl.pop() }.unwrap(), p2);
        assert_eq!(unsafe { fl.pop() }.unwrap(), p1);
        assert_eq!(unsafe { fl.pop() }.unwrap(), p0);
        assert!(unsafe { fl.pop() }.is_none());
    }
}

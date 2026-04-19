//! Top-level slab allocator.
//!
//! Routes `alloc`/`dealloc` calls to the right size-class cache.

use core::alloc::Layout;
use core::ptr::NonNull;

use crate::cache::Cache;
use crate::page_provider::PageProvider;

/// Supported object sizes (bytes), from smallest to largest.
///
/// # Examples
///
/// ```
/// use slab_allocator::SIZE_CLASSES;
///
/// assert_eq!(SIZE_CLASSES[0], 8);
/// assert_eq!(SIZE_CLASSES[8], 2048);
/// assert_eq!(SIZE_CLASSES.len(), 9);
/// ```
pub const SIZE_CLASSES: [usize; 9] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048];

const N_CLASSES: usize = SIZE_CLASSES.len();

/// Allocation statistics returned by [`SlabAllocator::stats`].
///
/// Useful for monitoring memory usage, debugging leaks, and verifying
/// that slab reclamation is working correctly.
///
/// # Examples
///
/// ```
/// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
/// use core::alloc::Layout;
///
/// let mut alloc = SlabAllocator::new(TestPageProvider::new());
/// let layout = Layout::from_size_align(32, 8).unwrap();
///
/// let ptr = alloc.alloc(layout);
/// let s = alloc.stats();
/// assert_eq!(s.alloc_count, 1);
/// assert_eq!(s.active_objects, 1);
///
/// unsafe { alloc.dealloc(ptr, layout) };
/// let s = alloc.stats();
/// assert_eq!(s.dealloc_count, 1);
/// assert_eq!(s.active_objects, 0);
/// ```
pub struct Stats {
    /// Total number of successful allocations since the allocator was created.
    pub alloc_count: usize,
    /// Total number of deallocations since the allocator was created.
    pub dealloc_count: usize,
    /// Number of currently live allocations (`alloc_count - dealloc_count`).
    pub active_objects: usize,
    /// Number of slabs currently in use across all caches.
    pub active_slabs: usize,
}

// SAFETY: SlabAllocator owns both the PageProvider pool and all NonNull pointers
// in its caches. Concurrent access must be serialised externally.
unsafe impl<P: PageProvider + Send> Send for SlabAllocator<P> {}

/// A multi-class slab allocator backed by any `PageProvider`.
///
/// # Usage
///
/// ```no_run
/// # use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
/// # use core::alloc::Layout;
/// let mut alloc = SlabAllocator::new(TestPageProvider::new());
/// let layout = Layout::from_size_align(16, 8).unwrap();
/// let ptr = alloc.alloc(layout);
/// assert!(!ptr.is_null());
/// unsafe { alloc.dealloc(ptr, layout) };
/// ```
pub struct SlabAllocator<P: PageProvider> {
    provider: P,
    caches: [Cache; N_CLASSES],
    alloc_count: usize,
    dealloc_count: usize,
}

impl<P: PageProvider> SlabAllocator<P> {
    /// Create a new allocator using `provider` as the page backend.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
    ///
    /// let alloc = SlabAllocator::new(TestPageProvider::new());
    /// ```
    pub fn new(provider: P) -> Self {
        let caches = core::array::from_fn(|i| Cache::new(SIZE_CLASSES[i], SIZE_CLASSES[i]));
        Self { provider, caches, alloc_count: 0, dealloc_count: 0 }
    }

    /// Find the cache index for a given `Layout`.
    ///
    /// Returns `None` if the size or alignment cannot be served.
    #[inline]
    fn class_index(layout: Layout) -> Option<usize> {
        let size = layout.size().max(1);
        let align = layout.align();
        let idx = SIZE_CLASSES.iter().position(|&sc| sc >= size)?;
        if align > SIZE_CLASSES[idx] {
            return None;
        }
        Some(idx)
    }

    /// Allocate memory satisfying `layout`.
    ///
    /// Returns a null pointer when the size/alignment is unsupported or on OOM.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
    /// use core::alloc::Layout;
    ///
    /// let mut alloc = SlabAllocator::new(TestPageProvider::new());
    /// let layout = Layout::from_size_align(32, 8).unwrap();
    ///
    /// let ptr = alloc.alloc(layout);
    /// assert!(!ptr.is_null());
    ///
    /// // OOM: size exceeds all size classes
    /// let big = Layout::from_size_align(4096, 8).unwrap();
    /// assert!(alloc.alloc(big).is_null());
    ///
    /// unsafe { alloc.dealloc(ptr, layout) };
    /// ```
    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let Some(idx) = Self::class_index(layout) else {
            return core::ptr::null_mut();
        };
        let provider = &mut self.provider;
        let cache = &mut self.caches[idx];
        match cache.alloc(provider) {
            Some(ptr) => {
                self.alloc_count += 1;
                ptr.as_ptr()
            }
            None => core::ptr::null_mut(),
        }
    }

    /// Deallocate a pointer previously returned by `self.alloc(layout)`.
    ///
    /// If the owning slab becomes empty its page is returned to the provider.
    /// A null pointer is silently ignored.
    ///
    /// # Safety
    ///
    /// - `ptr` must have been returned by `self.alloc(layout)` on this instance.
    /// - `ptr` must not be freed more than once.
    /// - `layout` must match the one passed to `alloc`.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
    /// use core::alloc::Layout;
    ///
    /// let mut alloc = SlabAllocator::new(TestPageProvider::new());
    /// let layout = Layout::from_size_align(16, 8).unwrap();
    ///
    /// let ptr = alloc.alloc(layout);
    /// assert!(!ptr.is_null());
    ///
    /// unsafe { alloc.dealloc(ptr, layout) };
    /// ```
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let Some(idx) = Self::class_index(layout) else {
            debug_assert!(false, "dealloc: layout {layout:?} not in any size class");
            return;
        };
        // SAFETY: ptr is non-null (checked above).
        let ptr = unsafe { NonNull::new_unchecked(ptr) };
        let provider = &mut self.provider;
        let cache = &mut self.caches[idx];
        // SAFETY: ptr came from cache.alloc() (caller guarantee).
        unsafe { cache.dealloc(ptr, provider) };
        self.dealloc_count += 1;
    }

    /// Total number of active slabs across all caches.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
    /// use core::alloc::Layout;
    ///
    /// let mut alloc = SlabAllocator::new(TestPageProvider::new());
    /// assert_eq!(alloc.total_slab_count(), 0);
    ///
    /// let layout = Layout::from_size_align(64, 8).unwrap();
    /// let ptr = alloc.alloc(layout);
    /// assert_eq!(alloc.total_slab_count(), 1); // one slab created
    ///
    /// unsafe { alloc.dealloc(ptr, layout) };
    /// assert_eq!(alloc.total_slab_count(), 0); // slab reclaimed
    /// ```
    pub fn total_slab_count(&self) -> usize {
        self.caches.iter().map(|c| c.slab_count()).sum()
    }

    /// Return current allocation statistics.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
    /// use core::alloc::Layout;
    ///
    /// let mut alloc = SlabAllocator::new(TestPageProvider::new());
    /// let layout = Layout::from_size_align(32, 8).unwrap();
    ///
    /// let ptr = alloc.alloc(layout);
    /// let s = alloc.stats();
    /// assert_eq!(s.alloc_count, 1);
    /// assert_eq!(s.active_objects, 1);
    ///
    /// unsafe { alloc.dealloc(ptr, layout) };
    /// let s = alloc.stats();
    /// assert_eq!(s.dealloc_count, 1);
    /// assert_eq!(s.active_objects, 0);
    /// ```
    pub fn stats(&self) -> Stats {
        Stats {
            alloc_count: self.alloc_count,
            dealloc_count: self.dealloc_count,
            active_objects: self.alloc_count.saturating_sub(self.dealloc_count),
            active_slabs: self.total_slab_count(),
        }
    }

    /// Mutable access to the underlying page provider.
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::page_provider::TestPageProvider;

    fn make_alloc() -> SlabAllocator<TestPageProvider> {
        SlabAllocator::new(TestPageProvider::new())
    }

    #[test]
    fn alloc_dealloc_basic() {
        let mut a = make_alloc();
        let layout = Layout::from_size_align(32, 8).unwrap();
        let p1 = a.alloc(layout);
        assert!(!p1.is_null());
        let p2 = a.alloc(layout);
        assert!(!p2.is_null());
        assert_ne!(p1, p2);
        unsafe { a.dealloc(p1, layout) };
        unsafe { a.dealloc(p2, layout) };
        assert_eq!(a.total_slab_count(), 0);
    }

    #[test]
    fn unsupported_size_returns_null() {
        let mut a = make_alloc();
        let layout = Layout::from_size_align(4096, 8).unwrap();
        assert!(a.alloc(layout).is_null());
    }

    #[test]
    fn oversized_alignment_returns_null() {
        let mut a = make_alloc();
        let layout = Layout::from_size_align(32, 64).unwrap();
        assert!(a.alloc(layout).is_null());
    }

    #[test]
    fn realloc_after_dealloc_succeeds() {
        // With slab coloring, a reclaimed+recreated slab may give a different
        // address. We verify the allocation simply succeeds.
        let mut a = make_alloc();
        let layout = Layout::from_size_align(16, 8).unwrap();
        let p = a.alloc(layout);
        assert!(!p.is_null());
        unsafe { a.dealloc(p, layout) };
        let q = a.alloc(layout);
        assert!(!q.is_null(), "realloc after dealloc should succeed");
    }

    #[test]
    fn realloc_within_live_slab_reuses_ptr() {
        // Keep an anchor alive to prevent slab reclamation.
        // The freed slot must be reused by the next alloc (freelist LIFO).
        let mut a = make_alloc();
        let layout = Layout::from_size_align(16, 8).unwrap();
        let _anchor = a.alloc(layout);
        let p = a.alloc(layout);
        assert!(!p.is_null());
        unsafe { a.dealloc(p, layout) };
        let q = a.alloc(layout);
        assert_eq!(p, q, "freed slot in live slab should be reused");
    }

    #[test]
    fn different_size_classes_are_independent() {
        let mut a = make_alloc();
        let l8 = Layout::from_size_align(8, 8).unwrap();
        let l64 = Layout::from_size_align(64, 8).unwrap();
        let p8 = a.alloc(l8);
        let p64 = a.alloc(l64);
        assert_ne!(p8, p64);
        unsafe {
            a.dealloc(p8, l8);
            a.dealloc(p64, l64);
        }
        assert_eq!(a.total_slab_count(), 0);
    }

    #[test]
    fn null_dealloc_is_a_nop() {
        let mut a = make_alloc();
        let layout = Layout::from_size_align(8, 8).unwrap();
        unsafe { a.dealloc(core::ptr::null_mut(), layout) };
    }

    #[test]
    fn stats_track_alloc_and_dealloc() {
        let mut a = make_alloc();
        let layout = Layout::from_size_align(32, 8).unwrap();

        assert_eq!(a.stats().alloc_count, 0);
        assert_eq!(a.stats().active_objects, 0);

        let p1 = a.alloc(layout);
        let p2 = a.alloc(layout);
        assert_eq!(a.stats().alloc_count, 2);
        assert_eq!(a.stats().active_objects, 2);

        unsafe { a.dealloc(p1, layout) };
        assert_eq!(a.stats().dealloc_count, 1);
        assert_eq!(a.stats().active_objects, 1);

        unsafe { a.dealloc(p2, layout) };
        assert_eq!(a.stats().active_objects, 0);
        assert_eq!(a.stats().active_slabs, 0);
    }
}

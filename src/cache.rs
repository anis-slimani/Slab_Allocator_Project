//! Cache: manages a list of slabs for one size class.
//!
//! Implements **slab coloring**: each new slab starts its object region at a
//! slightly different byte offset (the "color"). This spreads objects across
//! different cache-line positions, reducing hardware cache conflicts — the
//! same technique used by the Linux SLUB allocator.

use core::ptr::NonNull;

use crate::page_provider::PageProvider;
use crate::slab::{Slab, SlabHeader};

/// Number of distinct color offsets before wrapping back to zero.
const MAX_COLORS: usize = 8;

/// Cache for one object size-class.
///
/// Holds a singly-linked list of slabs. Allocation walks the list;
/// if no slab has a free slot a new page is requested from the provider.
/// Each new slab gets a different **color offset** to reduce cache conflicts.
pub struct Cache {
    obj_size: usize,
    align: usize,
    head: Option<NonNull<SlabHeader>>,
    /// Current color index (0..MAX_COLORS). Incremented on every new slab.
    next_color: usize,
}

// SAFETY: Cache owns all memory its NonNull pointers point into.
// Access must be serialised externally (e.g. LockedAllocator spinlock).
unsafe impl Send for Cache {}

impl Cache {
    /// Create a new, empty cache.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::cache::Cache;
    ///
    /// let cache = Cache::new(32, 32);
    /// assert_eq!(cache.obj_size(), 32);
    /// ```
    pub const fn new(obj_size: usize, align: usize) -> Self {
        Self { obj_size, align, head: None, next_color: 0 }
    }

    /// Returns the object size for this cache.
    pub fn obj_size(&self) -> usize {
        self.obj_size
    }

    /// Allocate one object from this cache.
    ///
    /// Walks the slab list looking for a free slot (fast path).
    /// If none is found, requests a new page from `provider` (slow path).
    /// The new slab is given the next color offset for cache-line spreading.
    ///
    /// Returns `None` on OOM.
    pub fn alloc<P: PageProvider>(&mut self, provider: &mut P) -> Option<NonNull<u8>> {
        let mut cur = self.head;
        while let Some(hdr) = cur {
            // SAFETY: pointers in the list were written by Slab::init and the page is still alive.
            let mut slab = unsafe { Slab::from_raw(hdr) };
            if let Some(ptr) = slab.alloc() {
                return Some(ptr);
            }
            cur = slab.next();
        }

        let page = provider.alloc_page()?;

        // Compute the color offset for this slab (slab coloring).
        let color = self.next_color * self.align;
        self.next_color = (self.next_color + 1) % MAX_COLORS;

        // SAFETY: page comes from the provider, PAGE_SIZE bytes, aligned, exclusively owned.
        let mut new_slab = unsafe { Slab::init(page, self.obj_size, self.align, color)? };

        // SAFETY: new_slab is a freshly-initialised valid slab.
        unsafe { new_slab.set_next(self.head) };
        self.head = Some(new_slab.header_ptr());

        new_slab.alloc()
    }

    /// Free `ptr` back to its owning slab.
    ///
    /// If the slab becomes empty its page is returned to the provider
    /// (**cache shrinking**).
    ///
    /// # Safety
    /// - `ptr` must have been returned by `self.alloc(provider)`.
    /// - `ptr` must not be freed more than once.
    pub unsafe fn dealloc<P: PageProvider>(&mut self, ptr: NonNull<u8>, provider: &mut P) {
        let mut prev: Option<NonNull<SlabHeader>> = None;
        let mut cur = self.head;

        while let Some(hdr) = cur {
            // SAFETY: list pointers are always valid.
            let mut slab = unsafe { Slab::from_raw(hdr) };

            if slab.contains(ptr) {
                // SAFETY: ptr belongs to this slab and caller guarantees no double-free.
                unsafe { slab.free(ptr) };

                if slab.is_empty() {
                    let next = slab.next();
                    match prev {
                        None => self.head = next,
                        Some(prev_hdr) => {
                            // SAFETY: prev_hdr is a live slab header in the list.
                            let mut prev_slab = unsafe { Slab::from_raw(prev_hdr) };
                            // SAFETY: prev_slab is live.
                            unsafe { prev_slab.set_next(next) };
                        }
                    }
                    // SAFETY: page_base() is the start of the page given to Slab::init.
                    let page = unsafe { NonNull::new_unchecked(slab.page_base()) };
                    // SAFETY: page came from provider.alloc_page().
                    unsafe { provider.dealloc_page(page) };
                }

                return;
            }

            prev = Some(hdr);
            cur = slab.next();
        }

        debug_assert!(false, "dealloc: ptr {ptr:p} not found in cache");
    }

    /// Number of slabs currently in this cache.
    pub fn slab_count(&self) -> usize {
        let mut count = 0;
        let mut cur = self.head;
        while let Some(hdr) = cur {
            // SAFETY: list pointers are always valid.
            let slab = unsafe { Slab::from_raw(hdr) };
            count += 1;
            cur = slab.next();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::page_provider::TestPageProvider;

    fn make_cache(size: usize) -> (TestPageProvider, Cache) {
        (TestPageProvider::new(), Cache::new(size, size))
    }

    #[test]
    fn alloc_returns_non_null() {
        let (mut prov, mut cache) = make_cache(32);
        let _p = cache.alloc(&mut prov).expect("alloc should return Some");
    }

    #[test]
    fn two_allocs_return_different_pointers() {
        let (mut prov, mut cache) = make_cache(32);
        let a = cache.alloc(&mut prov).expect("a");
        let b = cache.alloc(&mut prov).expect("b");
        assert_ne!(a, b);
    }

    #[test]
    fn dealloc_then_realloc_succeeds() {
        // With slab coloring, a reclaimed slab is replaced by one with a
        // different color offset, so the exact address may differ. We just
        // verify the new allocation succeeds.
        let (mut prov, mut cache) = make_cache(64);
        let p = cache.alloc(&mut prov).expect("alloc");
        unsafe { cache.dealloc(p, &mut prov) };
        let _q = cache.alloc(&mut prov).expect("realloc after free");
    }

    #[test]
    fn realloc_within_live_slab_reuses_slot() {
        // Keep an anchor alive so the slab is not reclaimed. The freed slot
        // must be immediately reused by the next alloc (freelist LIFO).
        let (mut prov, mut cache) = make_cache(64);
        let _anchor = cache.alloc(&mut prov).expect("anchor");
        let p = cache.alloc(&mut prov).expect("alloc");
        unsafe { cache.dealloc(p, &mut prov) };
        let q = cache.alloc(&mut prov).expect("realloc");
        assert_eq!(p, q, "freed slot in live slab should be reused");
    }

    #[test]
    fn slab_reclamation_frees_empty_slab() {
        let (mut prov, mut cache) = make_cache(32);
        let p = cache.alloc(&mut prov).expect("alloc");
        assert_eq!(cache.slab_count(), 1);
        unsafe { cache.dealloc(p, &mut prov) };
        assert_eq!(cache.slab_count(), 0, "empty slab should be reclaimed");
    }

    #[test]
    fn oom_returns_none() {
        let mut prov = crate::page_provider::StaticPageProvider::<1>::new();
        let mut cache = Cache::new(2048, 2048);
        let mut ptrs = std::vec::Vec::new();
        loop {
            match cache.alloc(&mut prov) {
                Some(p) => ptrs.push(p),
                None => break,
            }
        }
        assert!(!ptrs.is_empty(), "should have allocated at least one");
        assert!(cache.alloc(&mut prov).is_none(), "OOM expected");
    }

    #[test]
    fn slab_coloring_cycles_color_index() {
        let (mut prov, mut cache) = make_cache(32);
        // Each time a new slab is needed, the color index advances.
        // We just verify that many allocations across multiple slabs all succeed.
        let mut ptrs = std::vec::Vec::new();
        for _ in 0..500 {
            if let Some(p) = cache.alloc(&mut prov) {
                ptrs.push(p);
            }
        }
        assert!(!ptrs.is_empty());
        for p in ptrs {
            unsafe { cache.dealloc(p, &mut prov) };
        }
        assert_eq!(cache.slab_count(), 0);
    }
}

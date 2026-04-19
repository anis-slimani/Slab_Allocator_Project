//! Slab: one page (4096 bytes) sliced into equal-size objects.
//!
//! Layout:
//! ```text
//! [ SlabHeader | padding | color offset | obj0 | obj1 | ... | obj(cap-1) ]
//! ```
//! Free objects store a pointer to the next free object in their first bytes.
//! The optional color offset shifts where objects start to reduce cache conflicts.

use core::{mem, ptr::NonNull};

use crate::freelist::{FreeList, MIN_OBJ_SIZE};
use crate::page_provider::PAGE_SIZE;

/// Metadata stored at the start of every slab page.
#[repr(C)]
pub struct SlabHeader {
    /// Next slab in the cache list.
    pub(crate) next: Option<NonNull<SlabHeader>>,
    /// Free-list of available objects.
    freelist: FreeList,
    /// Number of objects currently allocated.
    inuse: u32,
    /// Total number of objects in this slab.
    capacity: u32,
    /// Size of one object (bytes).
    obj_size: u32,
    /// Alignment of objects (power of two).
    align: u32,
}

/// Handle to a slab — just a pointer to its header at the start of the page.
#[derive(Copy, Clone)]
pub struct Slab {
    hdr: NonNull<SlabHeader>,
}

impl Slab {
    /// Initialise a slab inside `page`.
    ///
    /// `color` is a byte offset added to the start of the data region to
    /// implement **slab coloring**: consecutive slabs start their objects at
    /// slightly different addresses, spreading accesses across cache lines and
    /// reducing cache conflicts (the same technique used by the Linux SLUB
    /// allocator).
    ///
    /// Returns `None` if no objects fit (should not happen with normal sizes).
    ///
    /// # Safety
    /// - `page` must point to `PAGE_SIZE` bytes of writable, exclusively owned memory.
    /// - `page` must remain valid for the lifetime of this slab.
    pub unsafe fn init(
        page: NonNull<u8>,
        obj_size: usize,
        align: usize,
        color: usize,
    ) -> Option<Self> {
        if !align.is_power_of_two() || align > PAGE_SIZE {
            return None;
        }

        let obj_size = align_up(obj_size.max(MIN_OBJ_SIZE), align);
        let base = page.as_ptr() as usize;
        let hdr_ptr = page.as_ptr().cast::<SlabHeader>();
        let hdr_size = mem::size_of::<SlabHeader>();
        let data_start = align_up(base + hdr_size, obj_size.max(align)) + color;

        if data_start >= base + PAGE_SIZE {
            return None;
        }

        let available = base + PAGE_SIZE - data_start;
        let capacity = available / obj_size;
        if capacity == 0 {
            return None;
        }

        // SAFETY: hdr_ptr is within the page (PAGE_SIZE bytes of writable memory).
        unsafe {
            hdr_ptr.write(SlabHeader {
                next: None,
                freelist: FreeList::new(),
                inuse: 0,
                capacity: capacity.min(u32::MAX as usize) as u32,
                obj_size: obj_size.min(u32::MAX as usize) as u32,
                align: align.min(u32::MAX as usize) as u32,
            })
        };

        let mut slab = Slab {
            // SAFETY: hdr_ptr is non-null (page is non-null).
            hdr: unsafe { NonNull::new_unchecked(hdr_ptr) },
        };

        let data_off = data_start - base;
        for i in (0..capacity).rev() {
            let obj_off = data_off + i * obj_size;
            // SAFETY: obj_off is within the page (derived from available space).
            let obj_ptr = unsafe { page.as_ptr().add(obj_off) };
            let obj = NonNull::new(obj_ptr).expect("non-null by construction");
            // SAFETY: obj is within the page, writable, not yet used.
            unsafe { slab.hdr.as_mut().freelist.push(obj) };
        }

        Some(slab)
    }

    /// Reconstruct a `Slab` from a raw header pointer.
    ///
    /// # Safety
    /// `hdr` must point to a `SlabHeader` written by `Slab::init` that is still alive.
    #[inline]
    pub unsafe fn from_raw(hdr: NonNull<SlabHeader>) -> Self {
        Self { hdr }
    }

    /// Allocate one object from this slab.
    ///
    /// Returns `None` when the slab is full.
    #[inline]
    pub fn alloc(&mut self) -> Option<NonNull<u8>> {
        // SAFETY: self.hdr is a live SlabHeader with valid freelist pointers.
        unsafe {
            let hdr = self.hdr.as_mut();
            let ptr = hdr.freelist.pop()?;
            hdr.inuse = hdr.inuse.saturating_add(1);
            Some(ptr)
        }
    }

    /// Return `ptr` to this slab's free-list.
    ///
    /// # Safety
    /// - `ptr` must have been obtained from `self.alloc()`.
    /// - `ptr` must not have been freed already (no double-free).
    #[inline]
    pub unsafe fn free(&mut self, ptr: NonNull<u8>) {
        // SAFETY: self.hdr is live; ptr is within the slab page and writable.
        unsafe {
            let hdr = self.hdr.as_mut();
            hdr.freelist.push(ptr);
            hdr.inuse = hdr.inuse.saturating_sub(1);
        }
    }

    /// Number of objects currently allocated.
    #[inline]
    pub fn inuse(&self) -> u32 {
        // SAFETY: hdr is a live SlabHeader.
        unsafe { self.hdr.as_ref().inuse }
    }

    /// Total capacity of this slab.
    #[inline]
    pub fn capacity(&self) -> u32 {
        // SAFETY: hdr is a live SlabHeader.
        unsafe { self.hdr.as_ref().capacity }
    }

    /// `true` when all objects have been freed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inuse() == 0
    }

    /// `true` when no more objects can be allocated.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.inuse() == self.capacity()
    }

    /// `true` if `ptr` lies within this slab's page.
    #[inline]
    pub fn contains(&self, ptr: NonNull<u8>) -> bool {
        let base = self.page_base() as usize;
        let p = ptr.as_ptr() as usize;
        p >= base && p < base + PAGE_SIZE
    }

    /// Pointer to the start of the page.
    #[inline]
    pub fn page_base(&self) -> *mut u8 {
        self.hdr.as_ptr().cast::<u8>()
    }

    /// Pointer to the slab header (used by Cache for the linked list).
    #[inline]
    pub fn header_ptr(&self) -> NonNull<SlabHeader> {
        self.hdr
    }

    /// Next slab in the cache list.
    #[inline]
    pub fn next(&self) -> Option<NonNull<SlabHeader>> {
        // SAFETY: hdr is a live SlabHeader.
        unsafe { self.hdr.as_ref().next }
    }

    /// Set the next-slab pointer.
    ///
    /// # Safety
    /// `self` must be a live, valid slab.
    #[inline]
    pub unsafe fn set_next(&mut self, next: Option<NonNull<SlabHeader>>) {
        // SAFETY: hdr is live.
        unsafe { self.hdr.as_mut().next = next };
    }
}

/// Round `x` up to the nearest multiple of `a` (must be a power of two).
///
/// # Examples
///
/// ```
/// use slab_allocator::align_up;
///
/// assert_eq!(align_up(0,  8),    0);   // already aligned
/// assert_eq!(align_up(1,  8),    8);   // rounded up
/// assert_eq!(align_up(8,  8),    8);   // already aligned
/// assert_eq!(align_up(9,  8),   16);   // rounded up
/// assert_eq!(align_up(32, 64),  64);   // rounded up to 64
/// assert_eq!(align_up(64, 64),  64);   // already aligned
/// assert_eq!(align_up(4095, 4096), 4096); // page alignment
/// ```
///
/// # Panics
///
/// Panics in debug builds if `a` is not a power of two.
#[inline]
pub fn align_up(x: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two(), "align_up: a must be a power of two");
    (x + (a - 1)) & !(a - 1)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::page_provider::{PageProvider, TestPageProvider};

    fn alloc_slab(obj_size: usize, align: usize) -> (TestPageProvider, Slab) {
        let mut prov = TestPageProvider::new();
        let page = prov.alloc_page().expect("page");
        let slab = unsafe { Slab::init(page, obj_size, align, 0).expect("slab init") };
        (prov, slab)
    }

    #[test]
    fn init_gives_nonzero_capacity() {
        let (_prov, slab) = alloc_slab(32, 8);
        assert!(slab.capacity() > 0);
    }

    #[test]
    fn alloc_returns_distinct_pointers() {
        let (_prov, mut slab) = alloc_slab(32, 8);
        let a = slab.alloc().expect("alloc A");
        let b = slab.alloc().expect("alloc B");
        assert_ne!(a, b, "same pointer returned twice");
    }

    #[test]
    fn alloc_pointers_are_aligned() {
        let align = 64usize;
        let (_prov, mut slab) = alloc_slab(64, align);
        let p = slab.alloc().expect("alloc");
        assert_eq!((p.as_ptr() as usize) % align, 0, "misaligned object");
    }

    #[test]
    fn alloc_pointers_are_within_page() {
        let (_prov, mut slab) = alloc_slab(32, 8);
        let p = slab.alloc().expect("alloc");
        assert!(slab.contains(p));
    }

    #[test]
    fn free_decrements_inuse() {
        let (_prov, mut slab) = alloc_slab(32, 8);
        let p = slab.alloc().expect("alloc");
        assert_eq!(slab.inuse(), 1);
        unsafe { slab.free(p) };
        assert_eq!(slab.inuse(), 0);
        assert!(slab.is_empty());
    }

    #[test]
    fn full_slab_returns_none() {
        let (_prov, mut slab) = alloc_slab(32, 8);
        let cap = slab.capacity();
        for _ in 0..cap {
            slab.alloc().expect("should not be full yet");
        }
        assert!(slab.is_full());
        assert!(slab.alloc().is_none(), "alloc on full slab should fail");
    }

    #[test]
    fn freed_slot_is_reused() {
        let (_prov, mut slab) = alloc_slab(32, 8);
        let p = slab.alloc().expect("first alloc");
        unsafe { slab.free(p) };
        let q = slab.alloc().expect("second alloc");
        assert_eq!(p, q, "freed slot should be reused");
    }

    #[test]
    fn coloring_does_not_break_allocation() {
        let mut prov = TestPageProvider::new();
        let page = prov.alloc_page().expect("page");
        let color = 8;
        let mut slab = unsafe { Slab::init(page, 32, 8, color).expect("colored slab") };
        let p = slab.alloc().expect("alloc from colored slab");
        assert!(slab.contains(p), "pointer must be within the page");
    }

    #[test]
    fn align_up_correct() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(4096, 4096), 4096);
    }
}

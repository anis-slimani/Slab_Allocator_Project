//! Page provider abstraction.
//!
//! The slab allocator delegates raw page allocation to a `PageProvider`.
//! This allows the same allocator code to work in bare-metal and hosted environments.

use core::cell::UnsafeCell;
use core::ptr::NonNull;

/// Size of one page in bytes.
///
/// # Examples
///
/// ```
/// use slab_allocator::page_provider::PAGE_SIZE;
/// assert_eq!(PAGE_SIZE, 4096);
/// ```
pub const PAGE_SIZE: usize = 4096;

/// Backend that hands out and recycles 4096-byte pages.
///
/// Implement this trait to plug in a custom memory source. The slab allocator
/// only requests whole pages and never partially uses them.
///
/// # Examples
///
/// ```
/// use core::ptr::NonNull;
/// use slab_allocator::page_provider::PageProvider;
///
/// struct OomProvider;
///
/// impl PageProvider for OomProvider {
///     fn alloc_page(&mut self) -> Option<NonNull<u8>> { None }
///     unsafe fn dealloc_page(&mut self, _ptr: NonNull<u8>) {}
/// }
///
/// let mut p = OomProvider;
/// assert!(p.alloc_page().is_none());
/// ```
pub trait PageProvider {
    /// Allocate one page (`PAGE_SIZE` bytes, aligned to `PAGE_SIZE`).
    ///
    /// Returns `None` when the backing memory is exhausted (OOM).
    fn alloc_page(&mut self) -> Option<NonNull<u8>>;

    /// Return a page previously obtained from `alloc_page`.
    ///
    /// # Safety
    /// - `ptr` must originate from *this* provider's `alloc_page`.
    /// - `ptr` must not have been freed already (no double-free).
    unsafe fn dealloc_page(&mut self, ptr: NonNull<u8>);
}

/// Page aligned to its own size (required for slab use).
#[repr(align(4096))]
#[derive(Copy, Clone)]
struct Page(#[allow(dead_code)] [u8; PAGE_SIZE]);

/// No-`std` page provider backed by a **compile-time-fixed** pool of `N` pages.
///
/// Pages live inside the struct itself (zero external allocation), which makes
/// this suitable for bare-metal kernels. The struct is `N × 4096` bytes, so
/// place it in a `static` rather than on the stack.
///
/// # Examples
///
/// ```
/// use slab_allocator::page_provider::{PageProvider, StaticPageProvider, PAGE_SIZE};
///
/// let mut p = StaticPageProvider::<2>::new();
/// let page = p.alloc_page().expect("page A");
/// assert_eq!((page.as_ptr() as usize) % PAGE_SIZE, 0);
/// unsafe { p.dealloc_page(page) };
/// ```
pub struct StaticPageProvider<const N: usize> {
    pool: [UnsafeCell<Page>; N],
    free_stack: [u16; N],
    free_len: usize,
}

impl<const N: usize> StaticPageProvider<N> {
    /// Create a provider with all `N` pages available.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::page_provider::{PageProvider, StaticPageProvider};
    ///
    /// let mut p = StaticPageProvider::<4>::new();
    /// let page = p.alloc_page().expect("should have pages");
    /// unsafe { p.dealloc_page(page) };
    /// ```
    pub fn new() -> Self {
        let mut free_stack = [0u16; N];
        let mut i = 0usize;
        while i < N {
            free_stack[i] = i as u16;
            i += 1;
        }
        Self {
            pool: core::array::from_fn(|_| UnsafeCell::new(Page([0u8; PAGE_SIZE]))),
            free_stack,
            free_len: N,
        }
    }

    fn page_ptr(&self, idx: usize) -> NonNull<u8> {
        let cell_ptr = core::ptr::addr_of!(self.pool[idx]);
        // SAFETY: idx < N, UnsafeCell::get is always non-null.
        let raw = unsafe { (*cell_ptr).get().cast::<u8>() };
        // SAFETY: raw is non-null (pool is a non-zero-sized array).
        unsafe { NonNull::new_unchecked(raw) }
    }

    fn index_from_ptr(&self, ptr: NonNull<u8>) -> Option<usize> {
        let base = self.page_ptr(0).as_ptr() as usize;
        let p = ptr.as_ptr() as usize;
        let total = N * core::mem::size_of::<Page>();
        if p < base || p >= base + total {
            return None;
        }
        let off = p - base;
        let page_size = core::mem::size_of::<Page>();
        if off % page_size != 0 {
            return None;
        }
        Some(off / page_size)
    }
}

impl<const N: usize> Default for StaticPageProvider<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PageProvider for StaticPageProvider<N> {
    fn alloc_page(&mut self) -> Option<NonNull<u8>> {
        if self.free_len == 0 {
            return None;
        }
        self.free_len -= 1;
        let idx = self.free_stack[self.free_len] as usize;
        let page = self.page_ptr(idx);
        // SAFETY: page points into pool[idx], exclusively owned (removed from free stack).
        unsafe { core::ptr::write_bytes(page.as_ptr(), 0, PAGE_SIZE) };
        Some(page)
    }

    unsafe fn dealloc_page(&mut self, ptr: NonNull<u8>) {
        let idx = match self.index_from_ptr(ptr) {
            Some(i) => i,
            None => {
                debug_assert!(false, "dealloc_page: ptr not from this pool");
                return;
            }
        };
        if self.free_len >= N {
            debug_assert!(false, "dealloc_page: free stack overflow (double-free?)");
            return;
        }
        self.free_stack[self.free_len] = idx as u16;
        self.free_len += 1;
    }
}

/// Heap-backed page provider, available when the `std` feature is enabled.
///
/// Each page is heap-allocated and tracked in a `Vec`. Useful in tests to
/// avoid large stack frames from `StaticPageProvider`.
#[cfg(any(test, feature = "std"))]
pub mod test_provider {
    extern crate std;

    use super::{PageProvider, PAGE_SIZE};
    use core::ptr::NonNull;
    use std::alloc::{alloc, dealloc, Layout};
    use std::vec::Vec;

    fn page_layout() -> Layout {
        Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("PAGE_SIZE is a valid alignment")
    }

    pub struct TestPageProvider {
        live: Vec<NonNull<u8>>,
    }

    impl TestPageProvider {
        pub fn new() -> Self {
            Self { live: Vec::new() }
        }
    }

    impl Default for TestPageProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl PageProvider for TestPageProvider {
        fn alloc_page(&mut self) -> Option<NonNull<u8>> {
            // SAFETY: layout is non-zero (PAGE_SIZE = 4096).
            let raw = unsafe { alloc(page_layout()) };
            let nn = NonNull::new(raw)?;
            self.live.push(nn);
            Some(nn)
        }

        unsafe fn dealloc_page(&mut self, ptr: NonNull<u8>) {
            let pos = self
                .live
                .iter()
                .position(|&p| p == ptr)
                .expect("dealloc_page: unknown or double-freed page");
            self.live.swap_remove(pos);
            // SAFETY: ptr came from alloc(page_layout()) and has not been freed yet.
            unsafe { dealloc(ptr.as_ptr(), page_layout()) };
        }
    }

    impl Drop for TestPageProvider {
        fn drop(&mut self) {
            while let Some(p) = self.live.pop() {
                // SAFETY: every pointer in live came from alloc(page_layout()).
                unsafe { dealloc(p.as_ptr(), page_layout()) };
            }
        }
    }
}

#[cfg(any(test, feature = "std"))]
pub use test_provider::TestPageProvider;

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    type Prov = TestPageProvider;

    #[test]
    fn alloc_is_page_aligned() {
        let mut p = Prov::new();
        let page = p.alloc_page().expect("alloc page");
        assert_eq!((page.as_ptr() as usize) % PAGE_SIZE, 0, "page must be PAGE_SIZE-aligned");
        unsafe { p.dealloc_page(page) };
    }

    #[test]
    fn two_pages_are_distinct() {
        let mut p = Prov::new();
        let a = p.alloc_page().expect("page a");
        let b = p.alloc_page().expect("page b");
        assert_ne!(a, b);
        unsafe {
            p.dealloc_page(a);
            p.dealloc_page(b);
        }
    }

    #[test]
    fn static_provider_oom_and_reuse() {
        let mut p = StaticPageProvider::<2>::new();
        let a = p.alloc_page().expect("page a");
        let b = p.alloc_page().expect("page b");
        assert!(p.alloc_page().is_none(), "OOM expected");
        unsafe { p.dealloc_page(a) };
        let c = p.alloc_page().expect("page c after free");
        assert_eq!((c.as_ptr() as usize) % PAGE_SIZE, 0);
        unsafe {
            p.dealloc_page(b);
            p.dealloc_page(c);
        }
    }
}

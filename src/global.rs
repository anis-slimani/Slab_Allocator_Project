//! Thread-safe global allocator wrapper.
//!
//! [`LockedAllocator`] wraps a [`SlabAllocator`] in a spinlock and implements
//! [`GlobalAlloc`] so it can be registered with `#[global_allocator]`.
//!
//! # Why interior mutability?
//!
//! [`GlobalAlloc::alloc`] takes `&self`, but [`SlabAllocator::alloc`] needs
//! `&mut self`. Wrapping the allocator in a [`Mutex`] solves this.
//!
//! # Usage in a `no_std` kernel
//!
//! ```no_run
//! use slab_allocator::global::LockedAllocator;
//!
//! #[global_allocator]
//! static ALLOCATOR: LockedAllocator<256> = LockedAllocator::new();
//!
//! fn kernel_main() {
//!     ALLOCATOR.init();
//!     // Box, Vec, etc. now work.
//! }
//! ```
//!
//! [`GlobalAlloc`]: core::alloc::GlobalAlloc
//! [`Mutex`]: crate::spinlock::Mutex

use core::alloc::{GlobalAlloc, Layout};

use crate::allocator::{SlabAllocator, Stats};
use crate::page_provider::StaticPageProvider;
use crate::spinlock::Mutex;

/// A thread-safe slab allocator that implements [`GlobalAlloc`].
///
/// `N` is the number of 4096-byte pages in the backing pool.
/// The allocator starts uninitialised — call [`init`] before any allocation.
///
/// # Examples
///
/// ```no_run
/// use slab_allocator::global::LockedAllocator;
///
/// #[global_allocator]
/// static ALLOCATOR: LockedAllocator<64> = LockedAllocator::new();
///
/// fn main() {
///     ALLOCATOR.init();
/// }
/// ```
///
/// [`GlobalAlloc`]: core::alloc::GlobalAlloc
/// [`init`]: LockedAllocator::init
pub struct LockedAllocator<const N: usize> {
    inner: Mutex<Option<SlabAllocator<StaticPageProvider<N>>>>,
}

impl<const N: usize> LockedAllocator<N> {
    /// Create an uninitialised allocator (safe to use in a `static`).
    ///
    /// Call [`init`] before allocating anything.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::global::LockedAllocator;
    ///
    /// static A: LockedAllocator<4> = LockedAllocator::new();
    /// ```
    ///
    /// [`init`]: LockedAllocator::init
    pub const fn new() -> Self {
        Self { inner: Mutex::new(None) }
    }

    /// Initialise the allocator with its page pool.
    ///
    /// Must be called once before any allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::global::LockedAllocator;
    ///
    /// static A: LockedAllocator<4> = LockedAllocator::new();
    /// A.init();
    /// ```
    pub fn init(&self) {
        let mut guard = self.inner.lock();
        *guard = Some(SlabAllocator::new(StaticPageProvider::new()));
    }

    /// Returns `true` if [`init`] has been called.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::global::LockedAllocator;
    ///
    /// let a: LockedAllocator<4> = LockedAllocator::new();
    /// assert!(!a.is_initialised());
    /// a.init();
    /// assert!(a.is_initialised());
    /// ```
    ///
    /// [`init`]: LockedAllocator::init
    pub fn is_initialised(&self) -> bool {
        self.inner.lock().is_some()
    }

    /// Return allocation statistics, or `None` if not yet initialised.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::global::LockedAllocator;
    /// use core::alloc::{GlobalAlloc, Layout};
    ///
    /// let a: LockedAllocator<4> = LockedAllocator::new();
    /// a.init();
    /// let layout = Layout::from_size_align(32, 8).unwrap();
    /// unsafe { a.alloc(layout) };
    /// let s = a.stats().unwrap();
    /// assert_eq!(s.alloc_count, 1);
    /// ```
    pub fn stats(&self) -> Option<Stats> {
        self.inner.lock().as_ref().map(|a| a.stats())
    }
}

impl<const N: usize> Default for LockedAllocator<N> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<const N: usize> GlobalAlloc for LockedAllocator<N> {
    /// Allocate memory for `layout`.
    ///
    /// Returns null when uninitialised, on OOM, or for unsupported layouts.
    ///
    /// # Safety
    ///
    /// The returned pointer must only be used within its allocated region
    /// and must be freed at most once via `dealloc`.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inner
            .lock()
            .as_mut()
            .map_or(core::ptr::null_mut(), |a| a.alloc(layout))
    }

    /// Return a previously allocated pointer.
    ///
    /// # Safety
    ///
    /// - `ptr` must have been returned by `self.alloc(layout)`.
    /// - `ptr` must not be freed more than once.
    /// - `layout` must match the one passed to `alloc`.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(a) = self.inner.lock().as_mut() {
            // SAFETY: caller guarantees ptr came from self.alloc(layout).
            unsafe { a.dealloc(ptr, layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::alloc::Layout;

    fn make_locked() -> LockedAllocator<8> {
        let a = LockedAllocator::new();
        a.init();
        a
    }

    #[test]
    fn starts_uninitialised() {
        let a: LockedAllocator<4> = LockedAllocator::new();
        assert!(!a.is_initialised());
    }

    #[test]
    fn after_init_is_initialised() {
        let a: LockedAllocator<4> = LockedAllocator::new();
        a.init();
        assert!(a.is_initialised());
    }

    #[test]
    fn alloc_before_init_returns_null() {
        let a: LockedAllocator<4> = LockedAllocator::new();
        let layout = Layout::from_size_align(8, 8).unwrap();
        // SAFETY: just checking null, we won't use the pointer.
        let ptr = unsafe { a.alloc(layout) };
        assert!(ptr.is_null(), "should return null before init");
    }

    #[test]
    fn alloc_and_dealloc_via_global_alloc_trait() {
        let a = make_locked();
        let layout = Layout::from_size_align(64, 8).unwrap();
        // SAFETY: layout is valid; we dealloc with the same layout.
        let ptr = unsafe { a.alloc(layout) };
        assert!(!ptr.is_null(), "alloc should succeed after init");
        unsafe { core::ptr::write_bytes(ptr, 0xAB, 64) };
        // SAFETY: ptr came from a.alloc(layout).
        unsafe { a.dealloc(ptr, layout) };
    }

    #[test]
    fn oom_returns_null() {
        let a: LockedAllocator<1> = LockedAllocator::new();
        a.init();
        let layout = Layout::from_size_align(2048, 8).unwrap();
        let p1 = unsafe { a.alloc(layout) };
        assert!(!p1.is_null());
        let p2 = unsafe { a.alloc(layout) };
        assert!(p2.is_null(), "OOM: second alloc must fail");
    }

    #[test]
    fn unsupported_layout_returns_null() {
        let a = make_locked();
        let layout = Layout::from_size_align(4096, 8).unwrap();
        let ptr = unsafe { a.alloc(layout) };
        assert!(ptr.is_null());
    }
}

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
}

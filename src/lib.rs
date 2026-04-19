#![cfg_attr(not(feature = "std"), no_std)]

pub mod freelist;
pub mod page_provider;
pub mod slab;
pub mod cache;
pub mod allocator;
pub mod spinlock;
pub mod global;

pub use allocator::SlabAllocator;
pub use allocator::Stats;
pub use page_provider::PageProvider;
pub use allocator::SIZE_CLASSES;
pub use slab::align_up;
pub use global::LockedAllocator;

/// Size of one page in bytes (4 096).
///
/// This constant is used throughout the allocator: one slab always occupies
/// exactly one page.
///
/// # Examples
///
/// ```
/// use slab_allocator::PAGE_SIZE;
/// assert_eq!(PAGE_SIZE, 4096);
/// ```
pub const PAGE_SIZE: usize = page_provider::PAGE_SIZE;

/// Return the index of the smallest size class that can hold `size` bytes.
///
/// Returns `None` when `size` exceeds the largest size class (2 048).
///
/// # Examples
///
/// ```
/// use slab_allocator::size_class_index;
///
/// assert_eq!(size_class_index(1),    Some(0)); // → class 8
/// assert_eq!(size_class_index(8),    Some(0)); // → class 8
/// assert_eq!(size_class_index(9),    Some(1)); // → class 16
/// assert_eq!(size_class_index(2048), Some(8)); // → class 2048
/// assert_eq!(size_class_index(2049), None);    // too large
/// ```
pub fn size_class_index(size: usize) -> Option<usize> {
    SIZE_CLASSES.iter().position(|&sc| sc >= size)
}

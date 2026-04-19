//! # Slab Allocator — From Linux Kernel to Rust OS
//!
//! A `no_std`-compatible slab allocator inspired by the Linux SLUB allocator,
//! with a [`GlobalAlloc`] wrapper for use as a `#[global_allocator]` in an OS
//! kernel.
//!
//! ## Architecture
//!
//! ```text
//! LockedAllocator<N>          ← implements GlobalAlloc (interior mutability)
//!   └── Mutex<SlabAllocator<StaticPageProvider<N>>>
//!         └── SlabAllocator<P>
//!               ├── Cache (size 8)   → [Slab] → [Slab] → …
//!               ├── Cache (size 16)  → …
//!               └── … (up to 2048)
//!
//! Each Slab (= 1 page, 4 096 bytes):
//!   ┌──────────────────────────────────────────────────┐
//!   │ SlabHeader (next, freelist, inuse, capacity, …)  │
//!   ├──────────────────────────────────────────────────┤
//!   │ object 0  │ object 1  │ …  │ object (cap-1)     │
//!   └──────────────────────────────────────────────────┘
//! Free objects store a pointer to the next free object
//! in their first bytes (intrusive free-list).
//! ```
//!
//! ## Quick start
//!
//! ### As a `#[global_allocator]` in a kernel
//!
//! ```no_run
//! use slab_allocator::global::LockedAllocator;
//!
//! #[global_allocator]
//! static ALLOCATOR: LockedAllocator<256> = LockedAllocator::new();
//!
//! fn kernel_main() {
//!     ALLOCATOR.init(); // must be called before any heap use
//!     // Box, Vec, String, … all work now.
//! }
//! ```
//!
//! ### Directly (without GlobalAlloc)
//!
//! ```
//! use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};
//! use core::alloc::Layout;
//!
//! let mut alloc = SlabAllocator::new(TestPageProvider::new());
//! let layout = Layout::from_size_align(32, 8).unwrap();
//!
//! let ptr = alloc.alloc(layout);
//! assert!(!ptr.is_null());
//! unsafe { alloc.dealloc(ptr, layout) };
//! ```
//!
//! ## Size classes
//!
//! | Size class | Max allocation |
//! |-----------|----------------|
//! | 8         | 1 – 8 bytes    |
//! | 16        | 9 – 16 bytes   |
//! | 32        | 17 – 32 bytes  |
//! | 64        | 33 – 64 bytes  |
//! | 128       | 65 – 128 bytes |
//! | 256       | 129 – 256 bytes|
//! | 512       | 257 – 512 bytes|
//! | 1024      | 513 – 1 024 bytes|
//! | 2048      | 1 025 – 2 048 bytes|
//!
//! Requests larger than **2 048 bytes** or with alignment exceeding the
//! selected size class return a **null pointer**.
//!
//! [`GlobalAlloc`]: core::alloc::GlobalAlloc

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


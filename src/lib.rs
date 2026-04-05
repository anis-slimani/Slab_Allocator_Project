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

//! Slab: one page (4096 bytes) sliced into equal-size objects.
//!
//! Layout:
//! ```text
//! [ SlabHeader | padding | obj0 | obj1 | ... | obj(capacity-1) ]
//! ```
//! Free objects store a pointer to the next free object in their first bytes.

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

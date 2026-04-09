//! Cache: manages a list of slabs for one size class.

use core::ptr::NonNull;

use crate::page_provider::PageProvider;
use crate::slab::{Slab, SlabHeader};

/// Cache for one object size-class.
///
/// Holds a singly-linked list of slabs. Allocation walks the list;
/// if no slab has a free slot a new page is requested from the provider.
pub struct Cache {
obj_size: usize,
align: usize,
head: Option<NonNull<SlabHeader>>,
}

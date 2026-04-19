//! Integration tests for `SlabAllocator`.
//!
//! All tests use `TestPageProvider` (heap-backed, one `std::alloc` call per
//! page) so that the test binary's stack never holds a large static pool.
//! This avoids the stack-overflow that occurs when `StaticPageProvider<64>`
//! (≈ 256 KB on the stack) is used inside threaded test runners.

extern crate std;

use core::alloc::Layout;
use slab_allocator::{page_provider::TestPageProvider, SlabAllocator};

fn make_alloc() -> SlabAllocator<TestPageProvider> {
    SlabAllocator::new(TestPageProvider::new())
}

// ─── Basic correctness ────────────────────────────────────────────────────────

#[test]
fn alloc_returns_non_null() {
    let mut a = make_alloc();
    let p = a.alloc(Layout::from_size_align(32, 8).unwrap());
    assert!(!p.is_null());
}

#[test]
fn two_allocs_return_distinct_pointers() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(32, 8).unwrap();
    let p1 = a.alloc(layout);
    let p2 = a.alloc(layout);
    assert!(!p1.is_null());
    assert!(!p2.is_null());
    assert_ne!(p1, p2);
}

#[test]
fn dealloc_then_alloc_reuses_pointer() {
    // Keep an anchor alive so the slab is not reclaimed after the first free.
    // Without reclamation, the freed slot is reused by the next alloc (LIFO).
    let mut a = make_alloc();
    let layout = Layout::from_size_align(16, 8).unwrap();

    let _anchor = a.alloc(layout);
    let p = a.alloc(layout);
    assert!(!p.is_null());
    unsafe { a.dealloc(p, layout) };

    let q = a.alloc(layout);
    assert_eq!(p, q, "freed slot in live slab should be immediately reused");
}

// ─── Size / alignment boundaries ─────────────────────────────────────────────

#[test]
fn unsupported_size_returns_null() {
    let mut a = make_alloc();
    // 2049 > largest size class (2048) → must fail.
    let layout = Layout::from_size_align(2049, 8).unwrap();
    assert!(a.alloc(layout).is_null());
}

#[test]
fn exactly_largest_class_works() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(2048, 8).unwrap();
    let p = a.alloc(layout);
    assert!(!p.is_null());
    unsafe { a.dealloc(p, layout) };
}

#[test]
fn alignment_larger_than_class_returns_null() {
    let mut a = make_alloc();
    // size=32 → class 32; align=64 > 32 → reject.
    let layout = Layout::from_size_align(32, 64).unwrap();
    assert!(a.alloc(layout).is_null());
}

#[test]
fn alignment_equal_to_class_succeeds() {
    let mut a = make_alloc();
    // size=64, align=64 → class 64; alignment is exactly the class size → ok.
    let layout = Layout::from_size_align(64, 64).unwrap();
    let p = a.alloc(layout);
    assert!(!p.is_null());
    assert_eq!((p as usize) % 64, 0, "pointer must be 64-byte aligned");
    unsafe { a.dealloc(p, layout) };
}

#[test]
fn null_dealloc_is_nop() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(8, 8).unwrap();
    // Must not panic.
    unsafe { a.dealloc(core::ptr::null_mut(), layout) };
}

// ─── Multi-object / multi-slab ────────────────────────────────────────────────

#[test]
fn alloc_many_then_dealloc_all() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(64, 8).unwrap();
    const N: usize = 200;

    let mut ptrs = [core::ptr::null_mut(); N];
    for slot in ptrs.iter_mut() {
        let p = a.alloc(layout);
        assert!(!p.is_null(), "alloc returned null unexpectedly");
        *slot = p;
    }

    // All pointers must be distinct.
    for i in 0..N {
        for j in (i + 1)..N {
            assert_ne!(ptrs[i], ptrs[j], "duplicate pointer at [{i}] and [{j}]");
        }
    }

    for &p in ptrs.iter() {
        unsafe { a.dealloc(p, layout) };
    }

    // After freeing everything all pages should be reclaimed.
    assert_eq!(
        a.total_slab_count(),
        0,
        "all slabs should be reclaimed after full dealloc"
    );
}

#[test]
fn cross_slab_dealloc_is_correct() {
    // Fill more than one slab worth of objects, then free them in reverse
    // order.  This exercises the case where `dealloc` must walk past the head
    // slab to find the owning slab.
    let mut a = make_alloc();
    let layout = Layout::from_size_align(8, 8).unwrap();

    // Allocate enough to span at least 2 slabs.
    let mut ptrs = std::vec::Vec::new();
    let mut last_base = usize::MAX;
    let mut found_second_slab = false;

    for _ in 0..2000 {
        let p = a.alloc(layout);
        assert!(!p.is_null());
        let base = (p as usize) & !(4095);
        if last_base != usize::MAX && base != last_base {
            found_second_slab = true;
        }
        last_base = base;
        ptrs.push(p);
    }

    assert!(found_second_slab, "should have spanned multiple slabs");

    for p in ptrs {
        unsafe { a.dealloc(p, layout) };
    }

    assert_eq!(a.total_slab_count(), 0);
}

// ─── Slab reclamation ─────────────────────────────────────────────────────────

#[test]
fn empty_slab_is_reclaimed() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(32, 8).unwrap();

    let p = a.alloc(layout);
    assert!(!p.is_null());
    assert_eq!(a.total_slab_count(), 1);

    unsafe { a.dealloc(p, layout) };
    assert_eq!(
        a.total_slab_count(),
        0,
        "slab should be reclaimed when empty"
    );
}

#[test]
fn partial_slab_is_not_reclaimed() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(32, 8).unwrap();

    let p1 = a.alloc(layout);
    let p2 = a.alloc(layout);
    assert!(!p1.is_null());
    assert!(!p2.is_null());

    // Free only one of the two → slab still has inuse > 0, must not be freed.
    unsafe { a.dealloc(p1, layout) };
    assert_eq!(
        a.total_slab_count(),
        1,
        "slab with live objects must not be reclaimed"
    );

    unsafe { a.dealloc(p2, layout) };
    assert_eq!(a.total_slab_count(), 0);
}

// ─── OOM ─────────────────────────────────────────────────────────────────────

#[test]
fn oom_returns_null() {
    // Use a 1-page static provider to trigger OOM predictably.
    use slab_allocator::page_provider::StaticPageProvider;

    // StaticPageProvider<1> is only ~4 KB on the stack — safe.
    let mut a = SlabAllocator::new(StaticPageProvider::<1>::new());
    let layout = Layout::from_size_align(2048, 8).unwrap();

    // The single page can hold exactly 1 object of size 2048 (after header).
    let first = a.alloc(layout);
    assert!(!first.is_null(), "first alloc should succeed");

    // Next alloc must fail: no pages left, and the existing slab is full.
    let second = a.alloc(layout);
    assert!(second.is_null(), "OOM should return null");
}

// ─── Memory write safety ──────────────────────────────────────────────────────

#[test]
fn allocated_memory_is_writable() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(64, 8).unwrap();

    let p = a.alloc(layout);
    assert!(!p.is_null());

    // Writing the full object must not corrupt anything.
    unsafe {
        core::ptr::write_bytes(p, 0xAB, 64);
        assert_eq!(*p, 0xAB);
    }

    unsafe { a.dealloc(p, layout) };
}

#[test]
fn all_size_classes_are_writable() {
    let mut a = make_alloc();
    use slab_allocator::SIZE_CLASSES;

    for &sc in SIZE_CLASSES.iter() {
        let layout = Layout::from_size_align(sc, 8).unwrap();
        let p = a.alloc(layout);
        assert!(!p.is_null(), "alloc failed for size class {sc}");
        unsafe {
            core::ptr::write_bytes(p, 0xFF, sc);
        }
        unsafe { a.dealloc(p, layout) };
    }
}

// ── Bonus: Statistics tracking ────────────────────────────────────────────────

#[test]
fn stats_alloc_count_increases() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(32, 8).unwrap();

    assert_eq!(a.stats().alloc_count, 0);
    assert_eq!(a.stats().active_objects, 0);

    let p1 = a.alloc(layout);
    let p2 = a.alloc(layout);
    assert_eq!(a.stats().alloc_count, 2);
    assert_eq!(a.stats().active_objects, 2);

    unsafe { a.dealloc(p1, layout) };
    unsafe { a.dealloc(p2, layout) };
    assert_eq!(a.stats().dealloc_count, 2);
    assert_eq!(a.stats().active_objects, 0);
}

#[test]
fn stats_active_slabs_matches_slab_count() {
    let mut a = make_alloc();
    let layout = Layout::from_size_align(64, 8).unwrap();

    let p = a.alloc(layout);
    assert_eq!(a.stats().active_slabs, a.total_slab_count());

    unsafe { a.dealloc(p, layout) };
    assert_eq!(a.stats().active_slabs, 0);
}

// ── Bonus: Slab coloring ──────────────────────────────────────────────────────

#[test]
fn slab_coloring_many_allocs_stay_valid() {
    // Allocate enough objects to trigger many new slabs (and thus many color
    // rotations). All pointers must be non-null, distinct, and writable.
    let mut a = make_alloc();
    let layout = Layout::from_size_align(32, 8).unwrap();
    let mut ptrs = std::vec::Vec::new();

    for _ in 0..400 {
        let p = a.alloc(layout);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 0xCC, 32) };
        ptrs.push(p);
    }

    for i in 0..ptrs.len() {
        for j in (i + 1)..ptrs.len() {
            assert_ne!(ptrs[i], ptrs[j], "duplicate pointer from colored slabs");
        }
    }

    for p in ptrs {
        unsafe { a.dealloc(p, layout) };
    }
    assert_eq!(a.total_slab_count(), 0);
}

// ── Bonus: GlobalAlloc trait demo ─────────────────────────────────────────────
//
// In a real no_std kernel, registering LockedAllocator as #[global_allocator]
// allows Box, Vec, String, etc. to work. The test below exercises the same
// alloc/dealloc code path that Box::new() uses internally.

#[test]
fn global_alloc_trait_alloc_and_dealloc() {
    use core::alloc::GlobalAlloc;
    use slab_allocator::LockedAllocator;

    let allocator: LockedAllocator<8> = LockedAllocator::new();
    allocator.init();

    let layout = Layout::from_size_align(64, 8).unwrap();

    // This is exactly what Box::new() does under the hood.
    let ptr = unsafe { allocator.alloc(layout) };
    assert!(!ptr.is_null(), "GlobalAlloc::alloc should succeed");

    unsafe { core::ptr::write_bytes(ptr, 0x42, 64) };
    assert_eq!(unsafe { *ptr }, 0x42);

    // And this is what Drop does when the Box is released.
    unsafe { allocator.dealloc(ptr, layout) };

    let s = allocator.stats().unwrap();
    assert_eq!(s.alloc_count, 1);
    assert_eq!(s.dealloc_count, 1);
    assert_eq!(s.active_objects, 0);
}

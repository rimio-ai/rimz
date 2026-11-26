//! The allocator QuickJS takes the engine's heap from: rquickjs's `RustAllocator`, counted, and
//! refused past the pass's memory cap, with the refusal recorded where no script can clear it.
//! The engine's own error for a refused allocation is an `InternalError` a script can catch, and
//! a script can make the same error itself, so the record here is what tells a pass it passed
//! the cap.

use std::cell::Cell;
use std::ptr;
use std::rc::Rc;

use rquickjs::allocator::{Allocator, RustAllocator};

/// What a pass's allocator counted: the bytes it handed out, the cap, and whether it refused.
pub(super) struct Heap {
    used: Cell<usize>,
    limit: Cell<usize>,
    refused: Cell<bool>,
}

impl Heap {
    /// An uncapped heap: a context's creation must not run out of memory, since QuickJS-NG
    /// 0.16.2 frees memory twice when it does. `cap` sets the limit once the context is built.
    pub(super) fn new() -> Rc<Self> {
        Rc::new(Self {
            used: Cell::new(0),
            limit: Cell::new(usize::MAX),
            refused: Cell::new(false),
        })
    }

    pub(super) fn cap(&self, limit: usize) {
        self.limit.set(limit);
    }

    /// Whether the allocator refused an allocation past the cap during this pass.
    pub(super) fn refused(&self) -> bool {
        self.refused.get()
    }

    /// Whether `more` bytes fit under the cap; a refusal is recorded.
    fn admit(&self, more: usize) -> bool {
        let fits = self
            .used
            .get()
            .checked_add(more)
            .is_some_and(|total| total <= self.limit.get());
        if !fits {
            self.refused.set(true);
        }
        fits
    }
}

/// `RustAllocator` behind the count of a `Heap`.
pub(super) struct Capped(pub(super) Rc<Heap>);

// SAFETY: every block comes from `RustAllocator` and goes back to it unchanged, so its
// contract holds as `RustAllocator`'s does; this type only counts sizes and returns null for an
// allocation past the cap before `RustAllocator` sees it. A null `realloc` pointer, which
// `RustAllocator` cannot take, is served as an allocation.
#[expect(
    unsafe_code,
    reason = "rquickjs's Allocator is an unsafe trait, and only an allocator of the engine's own \
              sees the refusal of an allocation past the cap"
)]
unsafe impl Allocator for Capped {
    fn alloc(&mut self, size: usize) -> *mut u8 {
        if !self.0.admit(size) {
            return ptr::null_mut();
        }
        let block = RustAllocator.alloc(size);
        if !block.is_null() {
            self.0
                .used
                .set(self.0.used.get() + unsafe { RustAllocator::usable_size(block) });
        }
        block
    }

    fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
        // `RustAllocator` panics on an overflowing product, across the C boundary.
        let Some(total) = count.checked_mul(size) else {
            return ptr::null_mut();
        };
        if !self.0.admit(total) {
            return ptr::null_mut();
        }
        let block = RustAllocator.calloc(count, size);
        if !block.is_null() {
            self.0
                .used
                .set(self.0.used.get() + unsafe { RustAllocator::usable_size(block) });
        }
        block
    }

    unsafe fn dealloc(&mut self, block: *mut u8) {
        let size = unsafe { RustAllocator::usable_size(block) };
        self.0.used.set(self.0.used.get() - size);
        unsafe { RustAllocator.dealloc(block) }
    }

    unsafe fn realloc(&mut self, block: *mut u8, new_size: usize) -> *mut u8 {
        if block.is_null() {
            return self.alloc(new_size);
        }
        let old = unsafe { RustAllocator::usable_size(block) };
        if !self.0.admit(new_size.saturating_sub(old)) {
            return ptr::null_mut();
        }
        let moved = unsafe { RustAllocator.realloc(block, new_size) };
        if !moved.is_null() {
            let new = unsafe { RustAllocator::usable_size(moved) };
            self.0.used.set(self.0.used.get() - old + new);
        }
        moved
    }

    unsafe fn usable_size(block: *mut u8) -> usize {
        unsafe { RustAllocator::usable_size(block) }
    }
}

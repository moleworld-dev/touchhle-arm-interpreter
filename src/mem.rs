/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Guest memory: typed guest pointers ([Ptr]) and a flat 4 GiB address space
//! ([Mem]).
//!
//! This mirrors the part of touchHLE's `src/mem.rs` that the interpreter needs.
//! The whole 32-bit (4 GiB) address space is reserved with a single `mmap` and
//! is **lazily committed** by the OS, so the resident memory footprint is only
//! the pages the guest actually touches (iPhone OS apps used 128–256 MiB). This
//! also means a guest address maps directly to `base + addr`, with no page table.
//!
//! Need a different backing store (a smaller [Vec], a custom MMU, snapshotting)?
//! Implement [GuestMem] for your own type — the interpreter only ever calls the
//! four methods on that trait. [Mem] is just the batteries-included default.

use std::ffi::c_void;
use std::marker::PhantomData;

/// Equivalent of `usize` for guest memory (the guest is 32-bit).
pub type GuestUSize = u32;
/// Equivalent of `isize` for guest memory.
pub type GuestISize = i32;
/// An untyped 32-bit guest virtual address.
pub type VAddr = GuestUSize;

/// [std::mem::size_of], but returning a [GuestUSize].
pub const fn guest_size_of<T: Sized>() -> GuestUSize {
    assert!(std::mem::size_of::<T>() <= u32::MAX as usize);
    std::mem::size_of::<T>() as u32
}

/// A guest pointer. `MUT` selects mutability; use the [ConstPtr] / [MutPtr] /
/// [ConstVoidPtr] / [MutVoidPtr] aliases rather than spelling out `MUT`.
///
/// It is just a typed 32-bit address — dereferencing happens through [Mem].
#[repr(transparent)]
pub struct Ptr<T, const MUT: bool>(VAddr, PhantomData<T>);

// Manual impls: derive would (wrongly) require `T: Trait`.
impl<T, const MUT: bool> Clone for Ptr<T, MUT> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T, const MUT: bool> Copy for Ptr<T, MUT> {}
impl<T, const MUT: bool> PartialEq for Ptr<T, MUT> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T, const MUT: bool> Eq for Ptr<T, MUT> {}
impl<T, const MUT: bool> std::hash::Hash for Ptr<T, MUT> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Constant guest pointer (like Rust's `*const T`).
pub type ConstPtr<T> = Ptr<T, false>;
/// Mutable guest pointer (like Rust's `*mut T`).
pub type MutPtr<T> = Ptr<T, true>;
/// Constant guest pointer-to-void (like C's `const void *`).
pub type ConstVoidPtr = ConstPtr<c_void>;
/// Mutable guest pointer-to-void (like C's `void *`).
pub type MutVoidPtr = MutPtr<c_void>;

impl<T, const MUT: bool> Ptr<T, MUT> {
    pub const fn null() -> Self {
        Ptr(0, PhantomData)
    }
    pub fn to_bits(self) -> VAddr {
        self.0
    }
    pub const fn from_bits(bits: VAddr) -> Self {
        Ptr(bits, PhantomData)
    }
    pub fn cast<U>(self) -> Ptr<U, MUT> {
        Ptr::<U, MUT>::from_bits(self.to_bits())
    }
    pub fn cast_void(self) -> Ptr<c_void, MUT> {
        self.cast()
    }
    pub fn is_null(self) -> bool {
        self.to_bits() == 0
    }
}

impl<T> ConstPtr<T> {
    pub fn cast_mut(self) -> MutPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}
impl<T> MutPtr<T> {
    pub fn cast_const(self) -> ConstPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}

impl<T, const MUT: bool> Default for Ptr<T, MUT> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T, const MUT: bool> std::fmt::Debug for Ptr<T, MUT> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "(null)")
        } else {
            write!(f, "{:#x}", self.to_bits())
        }
    }
}

// C-like pointer arithmetic (scaled by the pointee size).
impl<T, const MUT: bool> std::ops::Add<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;
    fn add(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_add(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::AddAssign<GuestUSize> for Ptr<T, MUT> {
    fn add_assign(&mut self, rhs: GuestUSize) {
        *self = *self + rhs;
    }
}
impl<T, const MUT: bool> std::ops::Sub<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;
    fn sub(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_sub(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::SubAssign<GuestUSize> for Ptr<T, MUT> {
    fn sub_assign(&mut self, rhs: GuestUSize) {
        *self = *self - rhs;
    }
}

/// Guest page size (iPhone OS used 4 KiB).
pub const PAGE_SIZE: GuestUSize = 4096;

/// The memory interface the interpreter depends on. [Mem] is the default
/// implementation; implement this yourself to use a custom backing store.
///
/// All accessors are byte-oriented and bounds-/null-checked. `addr` below is a
/// raw guest address wrapped in a [ConstVoidPtr] / [MutPtr]; use
/// [`Ptr::from_bits`] / [`Ptr::to_bits`] to convert.
pub trait GuestMem {
    /// Bytes below this address are the "null segment"; any access there is a
    /// guest null-pointer dereference and must fail.
    fn null_segment_size(&self) -> VAddr;
    /// Read-only view of `count` bytes at `addr`, or [None] if out of bounds or
    /// in the null segment.
    fn get_bytes_fallible(&self, addr: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]>;
    /// Mutable view of `count` bytes at `addr`, or [None] if out of bounds or in
    /// the null segment.
    fn get_bytes_fallible_mut(&mut self, addr: ConstVoidPtr, count: GuestUSize)
        -> Option<&mut [u8]>;
    /// Mutable view of `count` bytes; **panics** on a null-segment access (the
    /// infallible primitive, mirroring touchHLE's `bytes_at_mut`).
    fn bytes_at_mut(&mut self, ptr: MutPtr<u8>, count: GuestUSize) -> &mut [u8];
}

/// Total size of the guest address space: the full 32-bit range, 4 GiB.
const MEM_SIZE: usize = 1usize << 32;

/// Flat 4 GiB guest address space, reserved with one lazily-committed `mmap`.
///
/// Resident RAM is only the pages the guest touches. A guest address `a` maps to
/// host byte `base + a` directly (no page table). See the module docs.
pub struct Mem {
    /// Base of the 4 GiB reservation. Raw because we hand out interior slices and
    /// also expose the base to CPU backends; never let it dangle (see [Drop]).
    base: *mut u8,
    null_segment_size: VAddr,
}

// The interpreter drives one `Mem` from a single thread at a time. The raw `base`
// makes `Mem` `!Send`/`!Sync` by default; assert single-threaded ownership is the
// caller's responsibility (same contract as touchHLE).
unsafe impl Send for Mem {}

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    /// iPhone OS main-thread stack size (1 MiB), placed at the very top of the
    /// address space. Provided for convenience when setting up a guest stack.
    pub const MAIN_THREAD_STACK_SIZE: GuestUSize = 1024 * 1024;
    /// Lowest byte of the main-thread stack (top of the 4 GiB space minus 1 MiB).
    pub const MAIN_THREAD_STACK_LOW_END: VAddr =
        0u32.wrapping_sub(Self::MAIN_THREAD_STACK_SIZE);

    /// Reserve a fresh 4 GiB guest address space.
    pub fn new() -> Mem {
        // SAFETY: standard anonymous private mapping; MAP_NORESERVE keeps it from
        // counting against commit limits (pages fault in on first touch).
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                MEM_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(
            base != libc::MAP_FAILED && !base.is_null(),
            "failed to mmap 4 GiB guest address space"
        );
        Mem {
            base: base as *mut u8,
            null_segment_size: 0,
        }
    }

    /// Set the null-segment size (the low region that traps null derefs). Mirrors
    /// touchHLE's `__PAGE_ZERO`; typically set once during binary loading. Must be
    /// page-aligned.
    pub fn set_null_segment_size(&mut self, size: VAddr) {
        assert!(size % PAGE_SIZE == 0, "null segment must be page-aligned");
        self.null_segment_size = size;
    }

    /// Copy `data` into guest memory at `addr` (load code or data). Panics if the
    /// write would run past the 4 GiB space.
    pub fn write_bytes(&mut self, addr: VAddr, data: &[u8]) {
        let end = (addr as usize)
            .checked_add(data.len())
            .filter(|&e| e <= MEM_SIZE)
            .expect("write_bytes out of bounds");
        // SAFETY: bounds checked above; base owns the whole range.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(addr as usize), data.len());
        }
        let _ = end;
    }

    /// Read `count` bytes at `addr` as an owned [Vec] (convenience for tests).
    /// Panics on out-of-bounds.
    pub fn read_bytes_vec(&self, addr: VAddr, count: usize) -> Vec<u8> {
        let end = (addr as usize)
            .checked_add(count)
            .filter(|&e| e <= MEM_SIZE)
            .expect("read_bytes_vec out of bounds");
        let _ = end;
        // SAFETY: bounds checked.
        unsafe { std::slice::from_raw_parts(self.base.add(addr as usize), count).to_vec() }
    }

    /// Raw base pointer of the 4 GiB reservation. Only for CPU-backend setup that
    /// wants direct access; never hold this across a `&mut` on a guest region.
    ///
    /// # Safety
    /// The pointer must not outlive this [Mem].
    pub unsafe fn direct_memory_access_ptr(&mut self) -> *mut c_void {
        self.base.cast()
    }
}

impl GuestMem for Mem {
    fn null_segment_size(&self) -> VAddr {
        self.null_segment_size
    }

    fn get_bytes_fallible(&self, addr: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]> {
        let a = addr.to_bits();
        if a < self.null_segment_size {
            return None;
        }
        let end = (a as usize).checked_add(count as usize)?;
        if end > MEM_SIZE {
            return None;
        }
        // SAFETY: bounds + null checked; single-threaded access contract.
        Some(unsafe { std::slice::from_raw_parts(self.base.add(a as usize), count as usize) })
    }

    fn get_bytes_fallible_mut(
        &mut self,
        addr: ConstVoidPtr,
        count: GuestUSize,
    ) -> Option<&mut [u8]> {
        let a = addr.to_bits();
        if a < self.null_segment_size {
            return None;
        }
        let end = (a as usize).checked_add(count as usize)?;
        if end > MEM_SIZE {
            return None;
        }
        // SAFETY: bounds + null checked; single-threaded access contract.
        Some(unsafe { std::slice::from_raw_parts_mut(self.base.add(a as usize), count as usize) })
    }

    fn bytes_at_mut(&mut self, ptr: MutPtr<u8>, count: GuestUSize) -> &mut [u8] {
        let a = ptr.to_bits();
        if a < self.null_segment_size {
            panic!("attempted null-page access at {a:#x} ({count:#x} bytes)");
        }
        assert!(
            (a as usize).checked_add(count as usize).unwrap() <= MEM_SIZE,
            "bytes_at_mut out of bounds at {a:#x}"
        );
        // SAFETY: bounds + null checked.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(a as usize), count as usize) }
    }
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: base/size are exactly what we mmap'd.
        unsafe {
            libc::munmap(self.base.cast(), MEM_SIZE);
        }
    }
}

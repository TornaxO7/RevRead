use std::mem::{self, MaybeUninit};
use std::{cmp, ptr};

/// A borrowed byte buffer which is incrementally filled and initialized. This is basically just the reversed version of
/// [`std::io::BorrowedBuf`].
///
/// This type is a sort of "double cursor". It tracks three regions in the buffer:
/// - a region at the beginning of the buffer that is fully uninitialized
/// - a region that has been initialized at some point but not yet logically filled, and
/// - a region at the end that is fully initilized. The filled region is guaranteed to be a
/// subset of the initialized region.
///
/// In summary, the contents of the buffer can be visualized as:
/// ```not_rust
/// [             capacity              ]
/// [ unfilled |         filled         ]
/// [    uninitialized    | initialized ]
/// ```
///
/// A `RevBorrowedBuf` is created around some existing data (or capacity for data) via a unique reference
/// (`&mut`). The `RevBorrowedBuf` can be configured (e.g., using `clear` or `set_init`), but cannot be
/// directly written. To write into the buffer, use `unfilled` to create a `RevBorrowedCursor`. The cursor
/// has write-only access to the unfilled portion of the buffer (you can think of it as a
/// write-only iterator).
///
/// The lifetime `'data` is a bound on the lifetime of the underlying data.
#[derive(Debug)]
pub struct RevBorrowedBuf<'data> {
    /// The buffer's underlying data.
    buf: &'data mut [MaybeUninit<u8>],
    /// The starting index (inclusively) where the values are filled
    filled: usize,
    /// The starting index (inclusively) where the values are initialized
    init: usize,
}

/// Create a new `RevBorrowedBuf` from a fully initialized slice.
impl<'data> From<&'data mut [u8]> for RevBorrowedBuf<'data> {
    #[inline]
    fn from(slice: &'data mut [u8]) -> RevBorrowedBuf<'data> {
        let len = slice.len();

        RevBorrowedBuf {
            // SAFETY: initialized data never becoming uninitialized is an invariant of BorrowedBuf
            buf: unsafe { (slice as *mut [u8]).as_uninit_slice_mut().unwrap() },
            filled: len,
            init: 0,
        }
    }
}

/// Create a new `RevBorrowedBuf` from an uninitialized buffer.
///
/// Use `set_init` if part of the buffer is known to be already initialized.
impl<'data> From<&'data mut [MaybeUninit<u8>]> for RevBorrowedBuf<'data> {
    #[inline]
    fn from(buf: &'data mut [MaybeUninit<u8>]) -> RevBorrowedBuf<'data> {
        let len = buf.len();
        RevBorrowedBuf {
            buf,
            filled: len,
            init: len,
        }
    }
}

impl<'data> RevBorrowedBuf<'data> {
    /// Returns the total capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Returns the amount of bytes which are filled.
    #[inline]
    pub fn len(&self) -> usize {
        self.capacity() - self.filled
    }

    /// Returns `true` if the buf is empty, otherwise `false`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.len() == 0
    }

    /// Returns the amount of bytes of the initialized part of the buffer.
    #[inline]
    pub fn init_len(&self) -> usize {
        self.capacity() - self.init
    }

    /// Returns a shared reference to the filled portion of the buffer.
    #[inline]
    pub fn filled(&self) -> &[u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe { MaybeUninit::slice_assume_init_ref(&self.buf[self.filled..]) }
    }

    /// Returns a mutable reference to the filled portion of the buffer.
    #[inline]
    pub fn filled_mut(&mut self) -> &mut [u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe { MaybeUninit::slice_assume_init_mut(&mut self.buf[self.filled..]) }
    }

    /// Returns a cursor over the unfilled part of the buffer.
    #[inline]
    pub fn unfilled<'this>(&'this mut self) -> RevBorrowedCursor<'this> {
        RevBorrowedCursor {
            start: self.filled,
            // SAFETY: we never assign into `RevBorrowedCursor::buf`, so treating its
            // lifetime covariantly is safe.
            buf: unsafe {
                mem::transmute::<&'this mut RevBorrowedBuf<'data>, &'this mut RevBorrowedBuf<'this>>(
                    self,
                )
            },
        }
    }

    /// Clears the buffer, resetting the filled region to empty.
    ///
    /// The number of initialized bytes is not changed, and the contents of the buffer are not modified.
    #[inline]
    pub fn clear(&mut self) -> &mut Self {
        self.filled = self.capacity();
        self
    }

    /// Asserts that all bytes on the left (inclusive) to index `n` are initialised.
    ///
    /// `RevBorrowedBuf` assumes that bytes are never de-initialized, so this method does nothing when called with fewer
    /// bytes than are already known to be initialized.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the last `n` unfilled bytes of the buffer have already been initialized.
    #[inline]
    pub unsafe fn set_init(&mut self, n: usize) -> &mut Self {
        self.init = cmp::min(self.init, n);
        self
    }
}

/// A writeable view of the unfilled portion of a [`RevBorrowedBuf`](RevBorrowedBuf).
///
/// Provides access to the initialized and uninitialized parts of the underlying `RevBorrowedBuf`.
/// Data can be written directly to the cursor by using [`append`](RevBorrowedCursor::append) or
/// indirectly by getting a slice of part or all of the cursor and writing into the slice. In the
/// indirect case, the caller must call [`advance`](RevBorrowedCursor::advance) after writing to inform
/// the cursor how many bytes have been written.
///
/// Once data is written to the cursor, it becomes part of the filled portion of the underlying
/// `RevBorrowedBuf` and can no longer be accessed or re-written by the cursor. I.e., the cursor tracks
/// the unfilled part of the underlying `RevBorrowedBuf`.
///
/// The lifetime `'a` is a bound on the lifetime of the underlying buffer (which means it is a bound
/// on the data in that buffer by transitivity).
#[derive(Debug)]
pub struct RevBorrowedCursor<'a> {
    /// The underlying buffer.
    // Safety invariant: we treat the type of buf as covariant in the lifetime of `RevBorrowedBuf` when
    // we create a `BorrowedCursor`. This is only safe if we never replace `buf` by assigning into
    // it, so don't do that!
    buf: &'a mut RevBorrowedBuf<'a>,
    /// The length of the filled portion of the underlying buffer at the time of the cursor's
    /// creation.
    /// It applies: `self.buf.filled` <= `self.start`
    start: usize,
}

impl<'a> RevBorrowedCursor<'a> {
    /// Reborrow this cursor by cloning it with a smaller lifetime.
    ///
    /// Since a cursor maintains unique access to its underlying buffer, the borrowed cursor is
    /// not accessible while the new cursor exists.
    #[inline]
    pub fn reborrow<'this>(&'this mut self) -> RevBorrowedCursor<'this> {
        RevBorrowedCursor {
            // SAFETY: we never assign into `BorrowedCursor::buf`, so treating its
            // lifetime covariantly is safe.
            buf: unsafe {
                mem::transmute::<&'this mut RevBorrowedBuf<'a>, &'this mut RevBorrowedBuf<'this>>(
                    self.buf,
                )
            },
            start: self.start,
        }
    }

    /// Returns the available space in the cursor.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.filled
    }

    /// Returns the number of bytes written to this cursor since it was created from a `RevBorrowedBuf`.
    ///
    /// Note that if this cursor is a reborrowed clone of another, then the count returned is the
    /// count written via either cursor, not the count since the cursor was reborrowed.
    #[inline]
    pub fn written(&self) -> usize {
        self.start - self.buf.filled
    }

    /// Returns a shared reference to the initialized portion of the cursor.
    #[inline]
    pub fn init_ref(&self) -> &[u8] {
        debug_assert!(self.buf.init <= self.buf.filled);

        // SAFETY: We only slice the initialized part of the buffer, which is always valid
        unsafe { MaybeUninit::slice_assume_init_ref(&self.buf.buf[self.buf.init..]) }
    }

    /// Returns a mutable reference to the initialized portion of the cursor.
    #[inline]
    pub fn init_mut(&mut self) -> &mut [u8] {
        debug_assert!(self.buf.init <= self.buf.filled);

        // SAFETY: We only slice the initialized part of the buffer, which is always valid
        unsafe { MaybeUninit::slice_assume_init_mut(&mut self.buf.buf[self.buf.init..]) }
    }

    /// Returns a mutable reference to the uninitialized part of the cursor.
    ///
    /// It is safe to uninitialize any of these bytes.
    #[inline]
    pub fn uninit_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        &mut self.buf.buf[..self.buf.init]
    }

    /// Returns a mutable reference to the whole cursor.
    ///
    /// # Safety
    ///
    /// The caller must not uninitialize any bytes in the initialized portion of the cursor.
    #[inline]
    pub unsafe fn as_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        &mut self.buf.buf[..self.buf.filled]
    }

    /// Advance the cursor by asserting that `n` bytes have been filled.
    ///
    /// After advancing, the `n` bytes are no longer accessible via the cursor and can only be
    /// accessed via the underlying buffer. I.e., the buffer's filled portion grows by `n` elements
    /// and its unfilled portion (and the capacity of this cursor) shrinks by `n` elements.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` bytes of the cursor have been properly
    /// initialised.
    #[inline]
    pub fn advance(&mut self, n: usize) -> &mut Self {
        self.buf.filled -= n;
        self.buf.init = cmp::min(self.buf.init, self.buf.filled);
        self
    }

    /// Initializes all bytes in the cursor.
    #[inline]
    pub fn ensure_init(&mut self) -> &mut Self {
        let uninit = self.uninit_mut();
        // SAFETY: 0 is a valid value for MaybeUninit<u8> and the length matches the allocation
        // since it is comes from a slice reference.
        unsafe {
            ptr::write_bytes(uninit.as_mut_ptr(), 0, uninit.len());
        }
        self.buf.init = 0;

        self
    }

    /// Asserts that the first `n` unfilled bytes of the cursor are initialized.
    ///
    /// `RevBorrowedBuf` assumes that bytes are never de-initialized, so this method does nothing when
    /// called with fewer bytes than are already known to be initialized.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` bytes of the buffer have already been initialized.
    #[inline]
    pub unsafe fn set_init(&mut self, n: usize) -> &mut Self {
        self.buf.init = cmp::min(self.buf.init, self.buf.filled.saturating_sub(n));
        self
    }

    /// Appends data to the cursor, advancing position within its buffer.
    ///
    /// # Panics
    ///
    /// Panics if `self.capacity()` is less than `buf.len()`.
    #[inline]
    pub fn append(&mut self, buf: &[u8]) {
        assert!(self.capacity() >= buf.len());

        // SAFETY: we do not de-initialize any of the elements of the slice
        let mut_init_slice = unsafe { self.as_mut() };
        let mut_init_slice_len = mut_init_slice.len();
        MaybeUninit::copy_from_slice(
            &mut mut_init_slice[mut_init_slice_len.saturating_sub(buf.len())..],
            buf,
        );

        // SAFETY: We just added the entire contents of buf to the filled section.
        unsafe {
            self.set_init(buf.len());
        }
        self.buf.filled -= buf.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod rev_borrowed_buf {
        use super::*;

        #[test]
        fn filled() {
            let mut data = [1, 2, 3];
            let mut buf = RevBorrowedBuf::from(data.as_mut_slice());

            // assume, we filled one value
            buf.filled -= 1;
            assert_eq!(buf.filled(), [3]);

            // assume, we filled two values
            buf.filled -= 1;
            assert_eq!(buf.filled(), [2, 3]);
        }
    }

    mod rev_borrowed_cursor {
        use super::*;

        #[test]
        fn capacity() {
            let mut data = [1, 2, 3];
            let mut buf = RevBorrowedBuf::from(data.as_mut_slice());

            // assume, we filled one value
            buf.filled -= 1;
            let cursor = buf.unfilled();

            // one value has been written to in the buffer => at most 2 values can be written next
            assert_eq!(cursor.capacity(), 2);
        }

        #[test]
        fn append() {
            let mut buffer = [1, 2, 3];
            let mut buf = RevBorrowedBuf::from(buffer.as_mut_slice());

            let data = [4, 5];
            let mut cursor = buf.unfilled();
            cursor.append(&data);

            assert_eq!(cursor.written(), data.len());
            assert_eq!(cursor.init_ref(), [1, 4, 5]);
            assert_eq!(cursor.capacity(), 1);
        }

        #[test]
        #[should_panic]
        fn append_panic() {
            let mut buffer: [u8; 0] = [];
            let mut buf = RevBorrowedBuf::from(buffer.as_mut_slice());

            let data = [4, 5];
            let mut cursor = buf.unfilled();

            // capacity < data.len()!!!! => Panic
            cursor.append(&data);
        }
    }
}

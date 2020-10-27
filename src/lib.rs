//! A realtime-safe single-producer single-consumer ring buffer.
//!
//! Reading from and writing into the ring buffer is lock-free and wait-free.
//! All reading and writing functions return immediately.
//! Only a single thread can write into the ring buffer and a single thread
//! (typically a different one) can read from the ring buffer.
//! If the queue is empty, there is no way for the reading thread to wait
//! for new data, other than trying repeatedly until reading succeeds.
//! Similarly, if the queue is full, there is no way for the writing thread
//! to wait for newly available space to write to, other than trying repeatedly.
//!
//! A [`RingBuffer`] consists of two parts:
//! a [`Producer`] for writing into the ring buffer and
//! a [`Consumer`] for reading from the ring buffer.
//!
//! # Examples
//!
//! ```
//! use rtrb::RingBuffer;
//!
//! let (mut p, mut c) = RingBuffer::new(2).split();
//!
//! assert!(p.push(1).is_ok());
//! assert!(p.push(2).is_ok());
//! assert!(p.push(3).is_err());
//!
//! assert_eq!(c.pop(), Ok(1));
//! assert_eq!(c.pop(), Ok(2));
//! assert!(c.pop().is_err());
//! ```

#![warn(rust_2018_idioms)]
#![deny(missing_docs)]

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cache_padded::CachePadded;

mod error;

pub use error::{PeekError, PopError, PushError, SlicesError};

/// A bounded single-producer single-consumer queue.
pub struct RingBuffer<T> {
    /// The head of the queue.
    ///
    /// This integer is in range `0 .. 2 * capacity`.
    head: CachePadded<AtomicUsize>,

    /// The tail of the queue.
    ///
    /// This integer is in range `0 .. 2 * capacity`.
    tail: CachePadded<AtomicUsize>,

    /// The buffer holding slots.
    buffer: *mut T,

    /// The queue capacity.
    capacity: usize,

    /// Indicates that dropping a `Buffer<T>` may drop elements of type `T`.
    _marker: PhantomData<T>,
}

impl<T> RingBuffer<T> {
    /// Creates a [`RingBuffer`] with the given capacity.
    ///
    /// The returned [`RingBuffer`] is typically immediately split into
    /// the producer and the consumer side by [`RingBuffer::split()`].
    ///
    /// # Panics
    ///
    /// Panics if the capacity is zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let rb = RingBuffer::<f32>::new(100);
    /// ```
    /// Specifying an explicit type with the [turbofish](https://turbo.fish/)
    /// is is only necessary if it cannot be deduced by the compiler.
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let (mut p, c) = RingBuffer::new(100).split();
    /// assert!(p.push(0.0f32).is_ok());
    /// ```
    pub fn new(capacity: usize) -> RingBuffer<T> {
        assert!(capacity > 0, "capacity must be non-zero");

        // Allocate a buffer of length `capacity`.
        let buffer = {
            let mut v = Vec::<T>::with_capacity(capacity);
            let ptr = v.as_mut_ptr();
            mem::forget(v);
            ptr
        };
        RingBuffer {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            buffer,
            capacity,
            _marker: PhantomData,
        }
    }

    /// Splits the [`RingBuffer`] into [`Producer`] and [`Consumer`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let (p, c) = RingBuffer::<f32>::new(100).split();
    /// ```
    pub fn split(self) -> (Producer<T>, Consumer<T>) {
        let rb = Arc::new(self);
        let p = Producer {
            rb: rb.clone(),
            head: Cell::new(0),
            tail: Cell::new(0),
        };
        let c = Consumer {
            rb,
            head: Cell::new(0),
            tail: Cell::new(0),
        };
        (p, c)
    }

    /// Returns the capacity of the queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let rb = RingBuffer::<f32>::new(100);
    /// assert_eq!(rb.capacity(), 100);
    /// ```
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns a pointer to the slot at position `pos`.
    ///
    /// The position must be in range `0 .. 2 * capacity`.
    #[inline]
    unsafe fn slot(&self, pos: usize) -> *mut T {
        if pos < self.capacity {
            self.buffer.add(pos)
        } else {
            self.buffer.add(pos - self.capacity)
        }
    }

    /// Increments a position by going `n` slots forward.
    ///
    /// The position must be in range `0 .. 2 * capacity`.
    #[inline]
    fn increment(&self, pos: usize, n: usize) -> usize {
        let threshold = 2 * self.capacity - n;
        if pos < threshold {
            pos + n
        } else {
            pos - threshold
        }
    }

    /// Returns the distance between two positions.
    ///
    /// Positions must be in range `0 .. 2 * capacity`.
    #[inline]
    fn distance(&self, a: usize, b: usize) -> usize {
        if a <= b {
            b - a
        } else {
            2 * self.capacity - a + b
        }
    }
}

impl<T> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);

        // Loop over all slots that hold a value and drop them.
        while head != tail {
            unsafe {
                self.slot(head).drop_in_place();
            }
            head = self.increment(head, 1);
        }

        // Finally, deallocate the buffer, but don't run any destructors.
        unsafe {
            Vec::from_raw_parts(self.buffer, 0, self.capacity);
        }
    }
}

/// The producer side of a [`RingBuffer`].
///
/// Can be moved between threads,
/// but references from different threads are not allowed
/// (i.e. it is [`Send`] but not [`Sync`]).
///
/// Can only be created with [`RingBuffer::split()`].
///
/// # Examples
///
/// ```
/// use rtrb::RingBuffer;
///
/// let (producer, consumer) = RingBuffer::<f32>::new(1000).split();
/// ```
pub struct Producer<T> {
    /// The inner representation of the queue.
    rb: Arc<RingBuffer<T>>,

    /// A copy of `rb.head` for quick access.
    ///
    /// This value can be stale and sometimes needs to be resynchronized with `rb.head`.
    head: Cell<usize>,

    /// A copy of `rb.tail` for quick access.
    ///
    /// This value is always in sync with `rb.tail`.
    tail: Cell<usize>,
}

unsafe impl<T: Send> Send for Producer<T> {}

impl<T> Producer<T> {
    /// Attempts to push an element into the queue.
    ///
    /// If the queue is full, the element is returned back as an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::{RingBuffer, PushError};
    ///
    /// let (mut p, c) = RingBuffer::new(1).split();
    ///
    /// assert_eq!(p.push(10), Ok(()));
    /// assert_eq!(p.push(20), Err(PushError::Full(20)));
    /// ```
    pub fn push(&mut self, value: T) -> Result<(), PushError<T>> {
        if let Ok(tail) = self.get_tail(1) {
            unsafe {
                self.rb.slot(tail).write(value);
            }
            self.advance_tail(tail, 1);
            Ok(())
        } else {
            Err(PushError::Full(value))
        }
    }

    /// Returns the number of slots available for writing.
    pub fn slots(&self) -> usize {
        unimplemented!();
    }

    /// Returns `true` if the given number of slots is available for writing.
    fn _has_slots(&self, _n: usize) -> bool {
        unimplemented!();
    }

    /// Returns `true` if there are no slots available for writing.
    pub fn is_full(&self) -> bool {
        !self._has_slots(1)
    }

    /// Returns the capacity of the queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let (p, c) = RingBuffer::<f32>::new(100).split();
    /// assert_eq!(p.capacity(), 100);
    /// ```
    pub fn capacity(&self) -> usize {
        self.rb.capacity
    }

    /// Returns tail position on success, available slots on error.
    fn get_tail(&self, n: usize) -> Result<usize, usize> {
        let head = self.head.get();
        let tail = self.tail.get();

        // Check if the queue has *possibly* not enough slots.
        if self.rb.capacity - self.rb.distance(head, tail) < n {
            // Refresh the head ...
            let head = self.rb.head.load(Ordering::Acquire);
            self.head.set(head);

            // ... and check if there *really* are not enough slots.
            let slots = self.rb.capacity - self.rb.distance(head, tail);
            if slots < n {
                return Err(slots);
            }
        }
        Ok(tail)
    }

    fn advance_tail(&mut self, tail: usize, n: usize) {
        let tail = self.rb.increment(tail, n);
        self.rb.tail.store(tail, Ordering::Release);
        self.tail.set(tail);
    }
}

impl<T> Producer<T>
where
    T: Copy + Default,
{
    /*
    /// Returns mutable slices to the underlying buffer.
    ///
    /// `c.as_slices(c.slots())` never fails.
    /// `c.as_slices(0)` never fails (but is quite useless).
    pub fn as_mut_slices(&mut self, _n: usize) -> Result<(&mut [T], &mut [T]), SlicesError> {
        unimplemented!();
    }
    */

    /*
    /// Panics if `n` is larger than the number of available slots.
    pub fn advance(&mut self, _n: usize) {
        unimplemented!();
    }
    */
}

impl<T> fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Producer { .. }")
    }
}

/// The consumer side of a [`RingBuffer`].
///
/// Can be moved between threads,
/// but references from different threads are not allowed
/// (i.e. it is [`Send`] but not [`Sync`]).
///
/// Can only be created with [`RingBuffer::split()`].
///
/// # Examples
///
/// ```
/// use rtrb::RingBuffer;
///
/// let (producer, consumer) = RingBuffer::<f32>::new(1000).split();
/// ```
pub struct Consumer<T> {
    /// The inner representation of the queue.
    rb: Arc<RingBuffer<T>>,

    /// A copy of `rb.head` for quick access.
    ///
    /// This value is always in sync with `rb.head`.
    head: Cell<usize>,

    /// A copy of `rb.tail` for quick access.
    ///
    /// This value can be stale and sometimes needs to be resynchronized with `rb.tail`.
    tail: Cell<usize>,
}

unsafe impl<T: Send> Send for Consumer<T> {}

impl<T> Consumer<T> {
    /// Attempts to pop an element from the queue.
    ///
    /// If the queue is empty, an error is returned.
    ///
    /// To obtain an [`Option<T>`](std::option::Option),
    /// use [`.ok()`](std::result::Result::ok) on the result.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::{PopError, RingBuffer};
    ///
    /// let (mut p, mut c) = RingBuffer::new(1).split();
    ///
    /// assert_eq!(p.push(10), Ok(()));
    /// assert_eq!(c.pop(), Ok(10));
    /// assert_eq!(c.pop(), Err(PopError::Empty));
    ///
    /// assert_eq!(p.push(20), Ok(()));
    /// assert_eq!(c.pop().ok(), Some(20));
    /// ```
    pub fn pop(&mut self) -> Result<T, PopError> {
        if let Ok(head) = self.get_head(1) {
            let value = unsafe { self.rb.slot(head).read() };
            self.advance_head(head, 1);
            Ok(value)
        } else {
            Err(PopError::Empty)
        }
    }

    /// Attempts to read an element from the queue without removing it.
    ///
    /// If the queue is empty, an error is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::{PeekError, RingBuffer};
    ///
    /// let (mut p, c) = RingBuffer::new(1).split();
    ///
    /// assert_eq!(c.peek(), Err(PeekError::Empty));
    /// assert_eq!(p.push(10), Ok(()));
    /// assert_eq!(c.peek(), Ok(&10));
    /// assert_eq!(c.peek(), Ok(&10));
    /// ```
    pub fn peek(&self) -> Result<&T, PeekError> {
        if let Ok(head) = self.get_head(1) {
            Ok(unsafe { &*self.rb.slot(head) })
        } else {
            Err(PeekError::Empty)
        }
    }

    /// Returns the number of slots available for reading.
    pub fn slots(&self) -> usize {
        unimplemented!();
    }

    /// Returns `true` if the given number of slots is available for reading.
    fn _has_slots(&self, n: usize) -> bool {
        self.get_head(n).is_ok()
    }

    /// Returns `true` if there are no slots available for reading.
    pub fn is_empty(&self) -> bool {
        !self._has_slots(1)
    }

    /*
    /// Returns slices to the underlying buffer.
    ///
    /// `c.as_slices(c.slots())` never fails.
    /// `c.as_slices(0)` never fails (but is quite useless).
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::{RingBuffer, SlicesError};
    ///
    /// let (mut p, mut c) = RingBuffer::new(2).split();
    ///
    /// assert_eq!(c.as_slices(1), Err(SlicesError::TooFewSlots(0)));
    /// assert_eq!(p.push(10), Ok(()));
    /// assert_eq!(c.as_slices(99), Err(SlicesError::TooFewSlots(1)));
    /// assert_eq!(p.push(20), Ok(()));
    /// assert_eq!(c.as_slices(2), Ok(([10, 20].as_ref(), [].as_ref())));
    /// c.advance(1);
    /// assert_eq!(p.push(30), Ok(()));
    /// assert_eq!(c.as_slices(2), Ok(([20].as_ref(), [30].as_ref())));
    /// assert_eq!(c.as_slices(0), Ok(([].as_ref(), [].as_ref())));
    /// ```
    pub fn as_slices(&self, n: usize) -> Result<(&[T], &[T]), SlicesError> {
        let head = match self.get_head(n) {
            Ok(head) => head,
            Err(slots) => return Err(SlicesError::TooFewSlots(slots)),
        };
        let buffer_remainder = if head < self.rb.capacity {
            self.rb.capacity - head
        } else {
            2 * self.rb.capacity - head
        };
        let first_len = n.min(buffer_remainder);
        let first_slice = unsafe { std::slice::from_raw_parts(self.rb.slot(head), first_len) };
        let second_slice = unsafe { std::slice::from_raw_parts(self.rb.slot(0), n - first_len) };
        Ok((first_slice, second_slice))
    }
    */

    /// Returns slices for `n` slots.
    ///
    /// This does *not* advance the read position.
    ///
    /// If not enough slots are available for reading, an error is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::{RingBuffer, SlicesError};
    ///
    /// let (mut p, c) = RingBuffer::new(2).split();
    ///
    /// assert_eq!(p.push(10), Ok(()));
    /// assert_eq!(c.peek_slices(2), Err(SlicesError::TooFewSlots(1)));
    /// assert_eq!(p.push(20), Ok(()));
    ///
    /// if let Ok(slices) = c.peek_slices(2) {
    ///     assert_eq!(slices.first, [10, 20].as_ref());
    ///     assert_eq!(slices.second, [].as_ref());
    ///
    ///     let mut v = Vec::<i32>::new();
    ///     v.extend(slices); // slices implements IntoIterator!
    ///     assert_eq!(v, [10, 20]);
    /// } else {
    ///     unreachable!();
    /// }
    ///
    /// //c.advance(1);
    /// //assert_eq!(p.push(30), Ok(()));
    /// //assert_eq!(c.as_slices(2), Ok(([20].as_ref(), [30].as_ref())));
    /// //assert_eq!(c.as_slices(0), Ok(([].as_ref(), [].as_ref())));
    /// ```
    pub fn peek_slices(&self, n: usize) -> Result<PeekSlices<'_, T>, SlicesError> {
        let head = match self.get_head(n) {
            Ok(head) => head,
            Err(slots) => return Err(SlicesError::TooFewSlots(slots)),
        };

        // TODO: helper function collapse_position()?

        let buffer_remainder = if head < self.rb.capacity {
            self.rb.capacity - head
        } else {
            2 * self.rb.capacity - head
        };
        let first_len = n.min(buffer_remainder);
        Ok(PeekSlices {
            first: unsafe { std::slice::from_raw_parts(self.rb.slot(head), first_len) },
            second: unsafe { std::slice::from_raw_parts(self.rb.slot(0), n - first_len) },
        })
    }

    /// Returns slices for `n` slots, drops their contents when done and advances read position.
    ///
    /// If not enough slots are available for reading, an error is returned.
    ///
    /// If `T` implements `Copy`, [`Consumer::pop_slices()`] should be used instead.
    pub fn drop_slices(&mut self, _n: usize) -> Result<DropSlices, SlicesError> {
        unimplemented!();
    }

    /// Returns the capacity of the queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use rtrb::RingBuffer;
    ///
    /// let (p, c) = RingBuffer::<f32>::new(100).split();
    /// assert_eq!(c.capacity(), 100);
    /// ```
    pub fn capacity(&self) -> usize {
        self.rb.capacity
    }

    /// Returns head position on success, available slots on error.
    fn get_head(&self, n: usize) -> Result<usize, usize> {
        let head = self.head.get();
        let tail = self.tail.get();

        // Check if the queue has *possibly* not enough slots.
        if self.rb.distance(head, tail) < n {
            // Refresh the tail ...
            let tail = self.rb.tail.load(Ordering::Acquire);
            self.tail.set(tail);

            // ... and check if there *really* are not enough slots.
            let slots = self.rb.distance(head, tail);
            if slots < n {
                return Err(slots);
            }
        }
        Ok(head)
    }

    fn advance_head(&mut self, head: usize, n: usize) {
        let head = self.rb.increment(head, n);
        self.rb.head.store(head, Ordering::Release);
        self.head.set(head);
    }
}

impl<T> Consumer<T>
where
    T: Copy,
{
    /// Returns slices for `n` slots and advances read position when done.
    ///
    /// If not enough slots are available for reading, an error is returned.
    ///
    /// If `T` doesn't implement `Copy`, [`Consumer::drop_slices()`] can be used instead.
    pub fn pop_slices(&mut self, _n: usize) -> Result<PopSlices, SlicesError> {
        unimplemented!();
    }
}

/// Contains two slices from the ring buffer.
///
/// This is returned from [`Consumer::peek_slices()`].
///
/// It implements [`IntoIterator`] by chaining the two slices together,
/// and it can therefore, for example, be iterated with a `for` loop.
#[derive(Debug, PartialEq, Eq)]
pub struct PeekSlices<'a, T> {
    /// First part of the requested slots.
    ///
    /// Can only be empty if `0` slots have been requested.
    pub first: &'a [T],
    /// Second part of the requested slots.
    ///
    /// If `first` contains all requested slots, this is empty.
    pub second: &'a [T],
}

/// Contains two slices from the ring buffer. When this structure is dropped (falls out of scope),
/// the contents of the slices will be dropped and the read position will be advanced.
pub struct DropSlices {}

/// Contains two slices from the ring buffer. When this structure is dropped (falls out of scope),
/// the read position will be advanced.
pub struct PopSlices {}

impl<'a, T> IntoIterator for PeekSlices<'a, T> {
    type Item = &'a T;
    type IntoIter = std::iter::Chain<std::slice::Iter<'a, T>, std::slice::Iter<'a, T>>;
    fn into_iter(self) -> Self::IntoIter {
        self.first.iter().chain(self.second)
    }
}

/*
impl<T> Consumer<T>
where
    T: Copy,
{
    /// Panics if `n` is larger than the number of available slots.
    pub fn advance(&mut self, n: usize) {
        if let Ok(head) = self.get_head(n) {
            self.advance_head(head, n);
        } else {
            // TODO: better message
            // TODO: use match to get available slots from Err()?
            panic!("n is out of range");
        }
    }
}
*/

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Consumer { .. }")
    }
}

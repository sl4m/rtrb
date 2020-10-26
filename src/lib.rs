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
//! assert!(p.try_push(1).is_ok());
//! assert!(p.try_push(2).is_ok());
//! assert!(p.try_push(3).is_err());
//!
//! assert_eq!(c.try_pop(), Ok(1));
//! assert_eq!(c.try_pop(), Ok(2));
//! assert!(c.try_pop().is_err());
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

pub use error::{PopError, PushError, SlicesError};

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
    /// assert!(p.try_push(0.0f32).is_ok());
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
    /// assert_eq!(p.try_push(10), Ok(()));
    /// assert_eq!(p.try_push(20), Err(PushError::Full(20)));
    /// ```
    pub fn try_push(&mut self, value: T) -> Result<(), PushError<T>> {
        if let Some(tail) = self.get_tail(1) {
        unsafe {
            self.rb.slot(tail).write(value);
        }
            self.advance_tail(tail, 1);
            Ok(())
        } else {
            Err(PushError::Full(value))
        }
    }

    fn get_tail(&mut self, n: usize) -> Option<usize> {
        let head = self.head.get();
        let tail = self.tail.get();

        // Check if the queue has *possibly* not enough slots.
        if self.rb.distance(head, tail) + n > self.rb.capacity {
            // Refresh the head ...
            let head = self.rb.head.load(Ordering::Acquire);
            self.head.set(head);

            // ... and check if there *really* are not enough slots.
            if self.rb.distance(head, tail) + n > self.rb.capacity {
                return None;
            }
        }
        Some(tail)
    }

    fn advance_tail(&mut self, tail: usize, n:usize) {
        let tail = self.rb.increment(tail, n);
        self.rb.tail.store(tail, Ordering::Release);
        self.tail.set(tail);
    }

    /// Returns the number of slots available for writing.
    pub fn slots(&self) -> usize {
        unimplemented!();
    }

    /// Returns `true` if the given number of slots is available for writing.
    pub fn has_slots(&self, _n: usize) -> bool {
        unimplemented!();
    }

    /// Returns `true` if there are no slots available for writing.
    pub fn is_full(&self) -> bool {
        !self.has_slots(1)
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
}

impl<T> Producer<T>
where
    T: Copy + Default,
{
    /// Returns mutable slices to the underlying buffer.
    ///
    /// `c.as_slices(c.slots())` never fails.
    /// `c.as_slices(0)` never fails (but is quite useless).
    pub fn as_mut_slices(&mut self, _n: usize) -> Result<(&mut [T], &mut [T]), SlicesError> {
        unimplemented!();
    }

    /// Panics if `n` is larger than the number of available slots.
    pub fn advance(&mut self, _n: usize) {
        unimplemented!();
    }
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
    /// assert_eq!(p.try_push(10), Ok(()));
    /// assert_eq!(c.try_pop(), Ok(10));
    /// assert_eq!(c.try_pop(), Err(PopError::Empty));
    ///
    /// assert_eq!(p.try_push(20), Ok(()));
    /// assert_eq!(c.try_pop().ok(), Some(20));
    /// ```
    pub fn try_pop(&mut self) -> Result<T, PopError> {
        if let Some(head) = self.get_head(1) {
            let value = unsafe { self.rb.slot(head).read() };
            self.advance_head(head, 1);
            Ok(value)
        } else {
            Err(PopError::Empty)
        }
    }

    /// Returns the number of slots available for reading.
    pub fn slots(&self) -> usize {
        unimplemented!();
    }

    /// Returns `true` if the given number of slots is available for reading.
    pub fn has_slots(&self, _n: usize) -> bool {
        unimplemented!();
    }

    /// Returns `true` if there are no slots available for reading.
    pub fn is_empty(&self) -> bool {
        !self.has_slots(1)
    }

    /// Returns slices to the underlying buffer.
    ///
    /// `c.as_slices(c.slots())` never fails.
    /// `c.as_slices(0)` never fails (but is quite useless).
    pub fn as_slices(&self, _n: usize) -> Result<(&[T], &[T]), SlicesError> {
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

    fn get_head(&mut self, n: usize) -> Option<usize> {
        let head = self.head.get();
        let tail = self.tail.get();

        // Check if the queue has *possibly* not enough slots.
        if self.rb.distance(head, tail) < n {
            // Refresh the tail ...
            let tail = self.rb.tail.load(Ordering::Acquire);
            self.tail.set(tail);

            // ... and check if there *really* are not enough slots.
            if self.rb.distance(head, tail) < n {
                return None;
            }
        }
        Some(head)
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
    /// Panics if `n` is larger than the number of available slots.
    pub fn advance(&mut self, _n: usize) {
        unimplemented!();
    }
}

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Consumer { .. }")
    }
}

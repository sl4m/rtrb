use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use criterion::{black_box, Criterion};
use rtrb::RingBuffer;

pub fn push_and_pop(criterion: &mut Criterion) {
    criterion.bench_function("push-pop Vec", |b| {
        let mut v = Vec::with_capacity(1);
        let mut i: u8 = 0;
        b.iter(|| {
            v.push(black_box(i));
            assert_eq!(v.pop(), black_box(Some(i)));
            i = i.wrapping_add(black_box(1));
        })
    });

    criterion.bench_function("push-pop RingBuffer", |b| {
        let (p, c) = RingBuffer::new(1).split();
        let mut i: u8 = 0;
        b.iter(|| {
            p.push(black_box(i)).unwrap();
            assert_eq!(c.pop(), black_box(Ok(i)));
            i = i.wrapping_add(black_box(1));
        })
    });

    criterion.bench_function("push-pop via thread", |b| {
        let (p1, c1) = RingBuffer::new(1).split();
        let (p2, c2) = RingBuffer::new(1).split();
        let keep_thread_running = Arc::new(AtomicBool::new(true));
        let keep_running = Arc::clone(&keep_thread_running);

        let echo_thread = std::thread::spawn(move || {
            while keep_running.load(Ordering::Acquire) {
                if let Ok(x) = c1.pop() {
                    // NB: return channel will always be ready
                    p2.push(x).unwrap();
                }
            }
        });

        let mut i: u8 = 0;
        let result = b.iter(|| {
            while p1.push(black_box(i)).is_err() {}
            let x = loop {
                if let Ok(x) = c2.pop() {
                    break x;
                }
            };
            assert_eq!(x, black_box(i));
            i = i.wrapping_add(black_box(1));
        });

        keep_thread_running.store(false, Ordering::Release);
        echo_thread.join().unwrap();
        result
    });
}

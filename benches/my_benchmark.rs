use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rtrb::RingBuffer;

pub fn criterion_benchmark(criterion: &mut Criterion) {
    criterion.bench_function("write and read boxed variable", |b| {
        let mut v = Box::new(666);
        let mut i: usize = 0;
        b.iter(|| {
            *v = black_box(i);
            assert_eq!(*v, black_box(i));
            i += black_box(1);
        })
    });

    criterion.bench_function("push and pop Vec", |b| {
        let mut v = Vec::with_capacity(1);
        let mut i: usize = 0;
        b.iter(|| {
            v.push(black_box(i));
            assert_eq!(v.pop(), black_box(Some(i)));
            i += black_box(1);
        })
    });

    criterion.bench_function("push and pop 1-element RingBuffer", |b| {
        let (p, c) = RingBuffer::new(1).split();
        let mut i: usize = 0;
        b.iter(|| {
            p.push(black_box(i)).unwrap();
            assert_eq!(c.pop(), black_box(Ok(i)));
            i += black_box(1);
        })
    });

    criterion.bench_function("echo thread", |b| {
        let (p1, c1) = RingBuffer::new(1).split();
        let (p2, c2) = RingBuffer::new(1).split();
        let keep_thread_running = Arc::new(AtomicBool::new(true));
        let keep_running = Arc::clone(&keep_thread_running);

        let echo_thread = thread::spawn(move || {
            while keep_running.load(Ordering::Acquire) {
                match c1.pop() {
                    // NB: return channel will always be ready
                    Ok(x) => p2.push(x).unwrap(),
                    Err(_) => thread::yield_now(),
                }
            }
        });

        let mut i: usize = 0;
        let result = b.iter(|| {
            while p1.push(black_box(i)).is_err() {
                thread::yield_now();
            }
            let x = loop {
                match c2.pop() {
                    Ok(x) => break x,
                    Err(_) => thread::yield_now(),
                }
            };
            assert_eq!(x, black_box(i));
            i += black_box(1);
        });

        keep_thread_running.store(false, Ordering::Release);
        echo_thread.join().unwrap();
        result
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

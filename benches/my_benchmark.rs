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
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

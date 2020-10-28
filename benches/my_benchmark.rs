use criterion::{criterion_group, criterion_main, Criterion};
//use criterion::black_box;
use rtrb::RingBuffer;

pub fn criterion_benchmark(criterion: &mut Criterion) {
    criterion.bench_function("push and pop vec", |b| {
        let mut v = Vec::with_capacity(1);
        let mut i: usize = 0;
        b.iter(|| {
            v.push(i);
            assert_eq!(v.pop(), Some(i));
            i = i.wrapping_add(1);
        })
    });

    criterion.bench_function("push and pop rb", |b| {
        let (p, c) = RingBuffer::new(1).split();
        let mut i: usize = 0;
        b.iter(|| {
            p.push(i).unwrap();
            assert_eq!(c.pop(), Ok(i));
            i = i.wrapping_add(1);
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

use criterion::{criterion_group, criterion_main};

mod push_and_pop;

criterion_group!(benches, push_and_pop::push_and_pop);
criterion_main!(benches);

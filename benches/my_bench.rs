#![feature(test)]

extern crate test;

use test::Bencher;
//use test::black_box;

#[bench]
fn push_and_pop_vec(b: &mut Bencher) {
    let mut v = Vec::with_capacity(1);
    let mut i: usize = 0;
    b.iter(|| {
        v.push(i);
        assert_eq!(v.pop(), Some(i));
        i = i.wrapping_add(1);
    });
}

#[bench]
fn push_and_pop_rb(b: &mut Bencher) {
    let (p, c) = rtrb::RingBuffer::new(1).split();
    let mut i: usize = 0;
    b.iter(|| {
        p.push(i).unwrap();
        assert_eq!(c.pop(), Ok(i));
        i = i.wrapping_add(1);
    });
}

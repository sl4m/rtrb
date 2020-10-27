use std::thread;

use crossbeam_utils::thread::scope;
use rtrb::RingBuffer;

mod message;

const MESSAGES: usize = 5_000_000;

fn seq() {
    let (p, c) = RingBuffer::new(MESSAGES).split();

    for i in 0..MESSAGES {
        p.push(message::new(i)).unwrap();
    }

    for _ in 0..MESSAGES {
        c.pop().unwrap();
    }
}

fn spsc() {
    let (p, c) = RingBuffer::new(MESSAGES).split();

    scope(|scope| {
        scope.spawn(move |_| {
            for i in 0..MESSAGES {
                p.push(message::new(i)).unwrap();
            }
        });

        for _ in 0..MESSAGES {
            loop {
                if c.pop().is_err() {
                    thread::yield_now();
                } else {
                    break;
                }
            }
        }
    })
    .unwrap();
}

fn main() {
    macro_rules! run {
        ($name:expr, $f:expr) => {
            let now = ::std::time::Instant::now();
            $f;
            let elapsed = now.elapsed();
            println!(
                "{:25} {:15} {:7.3} sec",
                $name,
                "rtrb",
                elapsed.as_secs() as f64 + elapsed.subsec_nanos() as f64 / 1e9
            );
        };
    }

    run!("bounded_seq", seq());
    run!("bounded_spsc", spsc());
}

//! Single-producer / single-consumer sample ring.
//!
//! The real agent has three threads:
//!
//! ```text
//!   CoreAudio capture callback ──push──▶ [capture ring] ──pop──▶ denoise worker
//!   denoise worker ──push──▶ [render ring] ──pop──▶ CoreAudio playback callback
//! ```
//!
//! The audio callbacks run on a real-time thread owned by coreaudiod: they may
//! not allocate, may not block, and may not take a contended lock. So the
//! hand-off is a lock-free SPSC ring with fixed capacity, allocated once at
//! startup.
//!
//! Contract: `push`/`push_or_drop` are only ever called from the producer
//! thread, `pop`/`pop_or_silence` only from the consumer thread. Both may run
//! concurrently.
//!
//! The two spellings exist because "full" means two different things:
//!
//! * `push` / `pop` just move what fits and return the count. A caller that can
//!   afford to come back for the remainder (a feeder thread, a test) uses these,
//!   and the glitch counters stay clean.
//! * `push_or_drop` / `pop_or_silence` are the realtime spellings. The callback
//!   cannot retry and cannot leave a buffer half-written, so overflow is *lost*
//!   audio and starvation is *silence*; both are counted, which is what lets the
//!   run report tell "we glitched" apart from "we sounded fine".

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Capacity is rounded up to a power of two so masking replaces a modulo.
pub struct SpscRing {
    buf: Box<[UnsafeCell<f32>]>,
    mask: usize,
    head: AtomicUsize, // producer writes here (monotonic, wraps at 2^64 in practice never)
    tail: AtomicUsize, // consumer reads here
    overruns: AtomicUsize,
    underruns: AtomicUsize,
}

// Safety: `head` is only mutated by the producer, `tail` only by the consumer.
// Each side owns the half-open interval it writes / reads, and the Acquire /
// Release pairs below make the payload writes visible before the index update.
unsafe impl Sync for SpscRing {}
unsafe impl Send for SpscRing {}

impl SpscRing {
    /// `min_capacity` samples; rounded up to a power of two.
    pub fn new(min_capacity: usize) -> Self {
        let cap = min_capacity.next_power_of_two().max(2);
        let buf: Vec<UnsafeCell<f32>> = (0..cap).map(|_| UnsafeCell::new(0.0)).collect();
        Self {
            buf: buf.into_boxed_slice(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            overruns: AtomicUsize::new(0),
            underruns: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Samples currently queued.
    #[allow(dead_code)] // the report reads the counters, not the level
    pub fn len(&self) -> usize {
        self.head.load(Ordering::Acquire).wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    #[allow(dead_code)] // companion to `len`
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Producer side. Writes as much as fits and returns that count; the rest
    /// is left with the caller, who may retry. Nothing is counted, because
    /// nothing is known to be lost.
    pub fn push(&self, data: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.capacity() - head.wrapping_sub(tail);
        let n = data.len().min(free);
        for (i, &v) in data[..n].iter().enumerate() {
            let slot = (head.wrapping_add(i)) & self.mask;
            // Safety: this slot is in the producer-owned free region.
            unsafe { *self.buf[slot].get() = v };
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Realtime producer: whatever does not fit is *dropped* -- the callback has
    /// no way to come back for it -- and counted as an overrun.
    pub fn push_or_drop(&self, data: &[f32]) -> usize {
        let n = self.push(data);
        if n < data.len() {
            self.overruns.fetch_add(data.len() - n, Ordering::Relaxed);
        }
        n
    }

    /// Consumer side. Reads as many samples as are queued and returns that
    /// count; the caller decides what to do with the rest of `out`.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail);
        let n = out.len().min(avail);
        for (i, slot) in out[..n].iter_mut().enumerate() {
            let idx = (tail.wrapping_add(i)) & self.mask;
            // Safety: this slot is in the consumer-owned filled region.
            *slot = unsafe { *self.buf[idx].get() };
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// Realtime consumer: read what is queued, zero-fill the rest, and count
    /// the padding as starvation. This is what the render callback wants -- it
    /// must never leave the buffer uninitialised.
    pub fn pop_or_silence(&self, out: &mut [f32]) -> usize {
        let n = self.pop(out);
        if n < out.len() {
            self.underruns.fetch_add(out.len() - n, Ordering::Relaxed);
        }
        out[n..].fill(0.0);
        n
    }

    pub fn dropped_samples(&self) -> usize {
        self.overruns.load(Ordering::Relaxed)
    }

    pub fn starved_samples(&self) -> usize {
        self.underruns.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // lets a long run be split into windows
    pub fn reset_counters(&self) {
        self.overruns.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
    }

    /// Drop everything queued (used when the graph is (re)started).
    #[allow(dead_code)] // same as `DelayLine::reset`, not wired yet
    pub fn clear(&self) {
        let head = self.head.load(Ordering::Acquire);
        self.tail.store(head, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_order() {
        let r = SpscRing::new(8);
        assert_eq!(r.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(r.len(), 3);
        let mut out = [0.0f32; 3];
        assert_eq!(r.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert!(r.is_empty());
    }

    #[test]
    fn capacity_is_power_of_two_and_wraps() {
        let r = SpscRing::new(480);
        assert_eq!(r.capacity(), 512);
        let mut out = [0.0f32; 512];
        // fill, drain, fill again: the second pass must wrap the mask and still
        // come out in order
        let block: Vec<f32> = (0..512).map(|i| i as f32).collect();
        assert_eq!(r.push(&block), 512);
        assert_eq!(r.pop(&mut out), 512);
        assert_eq!(r.push(&block), 512);
        assert_eq!(r.pop(&mut out), 512);
        assert_eq!(&out[..], &block[..]);
    }

    #[test]
    fn overrun_drops_newest_and_counts() {
        let r = SpscRing::new(4); // capacity 4
        assert_eq!(r.push_or_drop(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4);
        assert_eq!(r.dropped_samples(), 2);
        let mut out = [0.0f32; 4];
        r.pop(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    /// A caller that retries a short write must not be reported as having
    /// dropped audio: that is the difference the run report depends on.
    #[test]
    fn retrying_push_does_not_count_as_a_drop() {
        let r = SpscRing::new(4); // capacity 4
        assert_eq!(r.push(&[1.0, 2.0, 3.0, 4.0]), 4);
        assert_eq!(r.push(&[5.0, 6.0]), 0, "full: nothing written");
        assert_eq!(r.dropped_samples(), 0, "a retryable write is not a drop");
        let mut out = [0.0f32; 2];
        assert_eq!(r.pop(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(r.push(&[5.0, 6.0]), 2, "the retry now fits");
        assert_eq!(r.dropped_samples(), 0);
    }

    /// ... and the realtime spelling, same buffer state, does count.
    #[test]
    fn push_or_drop_counts_what_it_had_to_leave_behind() {
        let r = SpscRing::new(4);
        assert_eq!(r.push_or_drop(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(r.push_or_drop(&[4.0, 5.0]), 1);
        assert_eq!(r.dropped_samples(), 1);
    }

    #[test]
    fn underrun_reports_and_pads() {
        let r = SpscRing::new(16);
        r.push(&[1.0, 2.0]);
        let mut out = [9.0f32; 5];
        assert_eq!(r.pop_or_silence(&mut out), 2);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0, 0.0]);
        assert_eq!(r.starved_samples(), 3);

        // the plain spelling pads nothing and, crucially, blames nobody
        let r2 = SpscRing::new(16);
        let before = r2.starved_samples();
        let mut out2 = [9.0f32; 5];
        assert_eq!(r2.pop(&mut out2), 0);
        assert_eq!(out2, [9.0, 9.0, 9.0, 9.0, 9.0], "pop leaves the caller's buffer alone");
        assert_eq!(r2.starved_samples(), before);
    }

    /// The actual threading contract: 100k samples through a 512-slot ring must
    /// come out in the same order with nothing lost.
    #[test]
    fn concurrent_producer_consumer_is_lossless() {
        use std::sync::Arc;
        let r = Arc::new(SpscRing::new(512));
        let n = 100_000usize;

        let producer = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                let mut written = 0usize;
                let mut v = 0.0f32;
                while written < n {
                    let chunk: Vec<f32> = (0..480).map(|_| { v += 1.0; v }).collect();
                    let take = chunk.len().min(n - written);
                    // spin on a full ring rather than dropping, so the test can
                    // assert losslessness
                    let mut off = 0;
                    while off < take {
                        let w = r.push(&chunk[off..take]);
                        if w == 0 {
                            std::hint::spin_loop();
                        }
                        off += w;
                    }
                    written += take;
                }
            })
        };

        let consumer = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                let mut got = Vec::with_capacity(n);
                let mut out = [0.0f32; 480];
                while got.len() < n {
                    let k = r.pop(&mut out);
                    if k == 0 {
                        std::hint::spin_loop();
                        continue;
                    }
                    got.extend_from_slice(&out[..k]);
                }
                got
            })
        };

        producer.join().unwrap();
        let got = consumer.join().unwrap();
        assert_eq!(got.len(), n);
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, (i + 1) as f32, "sample {i} out of order");
        }
        assert_eq!(r.dropped_samples(), 0);
        assert_eq!(r.starved_samples(), 0);
    }
}

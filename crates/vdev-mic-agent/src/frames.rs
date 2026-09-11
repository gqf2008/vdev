//! Block-size adaptation and the dry-path delay line.
//!
//! D1-D2 fed the model whole WAV files, so it could slice them into exact
//! 480-sample frames and delay the dry path by shifting one `Vec`. Neither
//! works in a live callback:
//!
//! * **Block size is not ours to choose.** CoreAudio hands us whatever the
//!   device's buffer size is (512 by default on the vdev device, 128-1024 in
//!   the wild, and it can change while running). RNNoise only accepts exactly
//!   480 samples. So the boundary has to be crossed without losing, duplicating
//!   or reordering a single sample -- `FrameAssembler`.
//! * **The dry path needs a delay *line*, not a shifted copy.** `mix < 1` and
//!   the adaptive gate both need the dry signal aligned to the model's 20 ms
//!   lookahead; in a streaming pipeline that is 960 samples of history, which
//!   has to be kept across callback boundaries -- `DelayLine`.
//!
//! Both are allocation-free after construction and are exercised by unit tests
//! that assert sample-exact equality with the offline (batch) behaviour, so the
//! live path cannot drift from the measured one.

use crate::rnnoise::FRAME;

/// Arbitrary-length input in, exact 480-sample frames out.
///
/// Nothing is dropped and nothing is padded: the assembler is a pure re-blocking
/// stage, so it contributes **zero** samples of latency. The first full frame
/// can only be emitted once 480 samples have arrived, which is inherent to
/// frame-based models, not something the assembler adds.
#[derive(Debug)]
pub struct FrameAssembler {
    buf: [f32; FRAME],
    len: usize,
    samples_in: u64,
    frames_out: u64,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAssembler {
    /// `const` so the live backend can keep the one assembler in a `static`:
    /// the capture callback may not allocate, and a lazily initialised global
    /// would need either a lock or `Once` on the audio thread.
    pub const fn new() -> Self {
        Self { buf: [0.0; FRAME], len: 0, samples_in: 0, frames_out: 0 }
    }

    /// Feed any number of samples; `emit` is called once per completed frame.
    ///
    /// The closure shape (instead of returning a `Vec`) is deliberate: this is
    /// called from the capture callback, where the denoise stage happens inline
    /// and no allocation may occur.
    #[inline]
    pub fn push<F: FnMut(&[f32])>(&mut self, input: &[f32], mut emit: F) {
        self.samples_in += input.len() as u64;
        let mut rest = input;
        while !rest.is_empty() {
            let take = (FRAME - self.len).min(rest.len());
            self.buf[self.len..self.len + take].copy_from_slice(&rest[..take]);
            self.len += take;
            rest = &rest[take..];
            if self.len == FRAME {
                self.frames_out += 1;
                emit(&self.buf);
                self.len = 0;
            }
        }
    }

    /// Samples buffered but not yet forming a frame (< 480).
    #[allow(dead_code)] // asserted by the tests; the live run does not poll it
    pub fn pending(&self) -> usize {
        self.len
    }

    /// Emit the trailing partial frame, zero-padded. Only for shutdown, where
    /// the alternative is clipping the tail of the recording.
    #[allow(dead_code)] // shutdown only: the live path never ends mid-frame
    pub fn flush<F: FnMut(&[f32])>(&mut self, mut emit: F) {
        if self.len > 0 {
            self.buf[self.len..].fill(0.0);
            self.frames_out += 1;
            emit(&self.buf);
            self.len = 0;
        }
    }

    #[allow(dead_code)] // counters for the report / the tests
    pub fn samples_in(&self) -> u64 {
        self.samples_in
    }

    #[allow(dead_code)] // counters for the report / the tests
    pub fn frames_out(&self) -> u64 {
        self.frames_out
    }
}

/// Fixed-length streaming delay.
///
/// `out[i] = in[i - delay]`, with `delay` samples of silence at the start. Used
/// to align the dry path to the model lookahead; without it the blend
/// comb-filters and SI-SDR collapses (measured: -7.7 dB, see the crate README).
#[derive(Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
    delay: usize,
}

impl DelayLine {
    pub fn new(delay: usize) -> Self {
        Self { buf: vec![0.0; delay.max(1)], pos: 0, delay }
    }

    #[allow(dead_code)] // introspection; the value is fixed at construction
    pub fn delay(&self) -> usize {
        self.delay
    }

    /// In-place delay of `x`. Allocation-free; safe to call from a callback.
    #[inline]
    pub fn process(&mut self, x: &mut [f32]) {
        if self.delay == 0 {
            return;
        }
        for v in x.iter_mut() {
            let old = self.buf[self.pos];
            self.buf[self.pos] = *v;
            self.pos += 1;
            if self.pos == self.delay {
                self.pos = 0;
            }
            *v = old;
        }
    }

    #[allow(dead_code)] // needed when the graph is (re)started, not yet wired
    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    /// The property the live path depends on: pushing in 512-sample device
    /// blocks yields exactly the same frames as slicing the file into 480s.
    #[test]
    fn assembler_is_a_pure_reblocking_stage() {
        let input = ramp(4800 + 137);
        let mut a = FrameAssembler::new();
        let mut got = Vec::new();
        for chunk in input.chunks(512) {
            a.push(chunk, |f| got.extend_from_slice(f));
        }
        assert_eq!(got, input[..4800].to_vec(), "frames must be the input, in order");
        assert_eq!(a.pending(), 137);
        assert_eq!(a.samples_in(), input.len() as u64);
        assert_eq!(a.frames_out(), 10);
    }

    /// Odd chunk sizes (1, 479, 481, 1000) must not lose or duplicate samples.
    #[test]
    fn assembler_survives_awkward_chunk_sizes() {
        for chunk in [1usize, 7, 479, 481, 512, 1000, 4096] {
            let input = ramp(480 * 5 + 3);
            let mut a = FrameAssembler::new();
            let mut got = Vec::new();
            for c in input.chunks(chunk) {
                a.push(c, |f| got.extend_from_slice(f));
            }
            assert_eq!(got.len(), 480 * 5, "chunk {chunk}");
            assert_eq!(got, input[..2400].to_vec(), "chunk {chunk}");
            a.flush(|f| got.extend_from_slice(f));
            assert_eq!(got.len(), 2880, "chunk {chunk}: flush pads to a full frame");
            assert_eq!(
                &got[2400..2403],
                &[2400.0, 2401.0, 2402.0],
                "chunk {chunk}: the partial frame keeps its samples"
            );
            assert!(
                got[2403..].iter().all(|&x| x == 0.0),
                "chunk {chunk}: and pads the rest with silence"
            );
            assert_eq!(a.pending(), 0, "chunk {chunk}: flush drains the assembler");
        }
    }

    /// A 46-sample remainder is preserved across pushes (no sample is silently
    /// consumed by a wrong carry).
    #[test]
    fn assembler_carries_partial_frame() {
        let mut a = FrameAssembler::new();
        let mut frames = 0;
        a.push(&ramp(46), |_| frames += 1);
        assert_eq!(frames, 0);
        assert_eq!(a.pending(), 46);
        a.push(&ramp(480 - 46), |_| frames += 1);
        assert_eq!(frames, 1);
        assert_eq!(a.pending(), 0);
    }

    /// Streaming delay must equal the batch shift `agent.rs` uses offline.
    #[test]
    fn delay_line_matches_batch_shift() {
        let input = ramp(3000);
        let lag = 960;

        // batch reference (what agent.rs does to a whole file)
        let mut batch = vec![0.0f32; input.len()];
        batch[lag..].copy_from_slice(&input[..input.len() - lag]);

        // streaming, in device-sized blocks
        let mut line = DelayLine::new(lag);
        let mut got = vec![0.0f32; input.len()];
        for (dst, src) in got.chunks_mut(512).zip(input.chunks(512)) {
            dst.copy_from_slice(src);
            line.process(dst);
        }
        assert_eq!(got, batch);
    }

    #[test]
    fn zero_delay_is_a_no_op() {
        let mut line = DelayLine::new(0);
        let mut x = vec![1.0f32, 2.0, 3.0];
        line.process(&mut x);
        assert_eq!(x, [1.0, 2.0, 3.0]);
    }

    /// After a reset the line starts its silence run again -- required when the
    /// graph is restarted so stale audio cannot leak into a new take.
    #[test]
    fn reset_clears_history() {
        let mut line = DelayLine::new(4);
        let mut x = vec![9.0f32; 8];
        line.process(&mut x);
        line.reset();
        let mut y = vec![9.0f32; 4];
        line.process(&mut y);
        assert_eq!(y, [0.0, 0.0, 0.0, 0.0]);
    }
}

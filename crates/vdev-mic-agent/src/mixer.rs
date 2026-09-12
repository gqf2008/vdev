//! Adaptive dry/wet mixing.
//!
//! The problem it solves: RNNoise attenuates *every* frame, including frames
//! that were already clean. Pushing a perfectly clean recording through it
//! costs SI-SDR (measured: 14.7 dB, with a faint metallic colour) for no
//! benefit at all. On a laptop with a decent mic and a quiet room that is a
//! pure loss.
//!
//! Policy -- two estimators, one gain:
//!
//!   1. **Noise floor.** Only *non-speech* frames (VAD below `vad_thresh`)
//!      update it, so a long sentence can never drag the floor up and hide the
//!      noise. It falls instantly and rises slowly: a minimum tracker.
//!   2. **Speech level.** One-pole average of the energy of *speech* frames.
//!
//! From those, a long-term SNR. The gain follows it through a hysteresis gate:
//! a mic that is 30+ dB above its own noise floor is left alone (`min_wet`),
//! anything below ~20 dB gets the full model (`max_wet`).
//!
//! Everything is deliberately slow (hundreds of ms). A per-frame SNR would flip
//! the gain on every 10 ms boundary and pump audibly; what we want is "is this
//! room noisy?", which is a property of the room, not of the frame.

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveConfig {
    /// Long-term SNR above which the frame counts as "already clean".
    pub gate_db: f64,
    /// Hysteresis around the gate, to stop it oscillating at the threshold.
    pub hysteresis_db: f64,
    /// Width of the linear ramp below the gate, in dB.
    pub ramp_db: f64,
    /// Wet ratio when the mic is clean.
    pub min_wet: f64,
    /// Wet ratio when the mic is noisy.
    pub max_wet: f64,
    /// VAD probability below which a frame counts as non-speech.
    pub vad_thresh: f32,
    /// One-pole coefficient when the wet ratio must rise (noise appeared).
    pub attack: f64,
    /// One-pole coefficient when it must fall (the room is clean).
    pub release: f64,
    /// Noise-floor fall coefficient (frame is quieter than the floor).
    pub floor_fall: f64,
    /// Noise-floor rise coefficient (frame is louder than the floor).
    pub floor_rise: f64,
    /// One-pole coefficient for the speech-level estimate.
    pub speech_smooth: f64,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            gate_db: 25.0,
            hysteresis_db: 2.0,
            ramp_db: 10.0,
            min_wet: 0.0,
            max_wet: 1.0,
            vad_thresh: 0.30,
            attack: 0.20,
            release: 0.01,
            floor_fall: 0.50,
            floor_rise: 0.01,
            speech_smooth: 0.02,
        }
    }
}

#[derive(Debug)]
pub struct AdaptiveMixer {
    cfg: AdaptiveConfig,
    noise_floor: f64,
    speech_level: f64,
    wet: f64,
    have_speech: bool,
    initialised: bool,
    pub enabled: bool,
}

impl AdaptiveMixer {
    pub fn new(enabled: bool) -> Self {
        Self {
            cfg: AdaptiveConfig::default(),
            noise_floor: 1e-9,
            speech_level: 1e-9,
            wet: 1.0,
            have_speech: false,
            initialised: false,
            enabled,
        }
    }

    /// Feed one input frame (int16 scale) and the VAD probability the model
    /// returned for that same frame; get the wet ratio to use for it.
    pub fn update(&mut self, frame: &[f32], vad: f32) -> f64 {
        if !self.enabled {
            return 1.0;
        }
        let e =
            frame.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / frame.len().max(1) as f64;

        if !self.initialised {
            // Start pessimistic: assume the first frame is the worst case.
            self.noise_floor = e.max(1e-9);
            self.speech_level = e.max(1e-9);
            self.initialised = true;
        } else if vad < self.cfg.vad_thresh {
            let k = if e < self.noise_floor {
                self.cfg.floor_fall
            } else {
                self.cfg.floor_rise
            };
            self.noise_floor = (self.noise_floor + (e - self.noise_floor) * k).max(1e-9);
        } else if e > self.noise_floor {
            if self.have_speech {
                self.speech_level += (e - self.speech_level) * self.cfg.speech_smooth;
            } else {
                self.speech_level = e;
                self.have_speech = true;
            }
        }

        let snr_db = 10.0 * (self.speech_level / self.noise_floor).max(1e-12).log10();
        let c = &self.cfg;
        // hysteresis: a higher bar to *become* wet than to *stay* wet.
        // Dry state (wet < 0.5) uses the *lower* gate, so dropping into wet
        // needs the SNR to fall below gate_db - hysteresis_db; the wet state
        // keeps the *higher* gate, so only SNR >= gate_db + hysteresis_db
        // counts as clean again. Inside the band the state is stable -- a
        // constant SNR can never flip-flop the gate (no limit cycle).
        let gate = if self.wet < 0.5 {
            c.gate_db - c.hysteresis_db
        } else {
            c.gate_db + c.hysteresis_db
        };
        let target = if snr_db >= gate {
            c.min_wet
        } else if snr_db <= gate - c.ramp_db {
            c.max_wet
        } else {
            let t = (gate - snr_db) / c.ramp_db; // 0 = clean, 1 = noisy
            c.min_wet + t * (c.max_wet - c.min_wet)
        };

        let k = if target > self.wet {
            c.attack
        } else {
            c.release
        };
        self.wet = (self.wet + (target - self.wet) * k).clamp(0.0, 1.0);
        self.wet
    }

    pub fn noise_floor_dbfs(&self) -> f64 {
        20.0 * (self.noise_floor.max(1e-12).sqrt() / 32768.0).log10()
    }

    pub fn long_term_snr_db(&self) -> f64 {
        10.0 * (self.speech_level / self.noise_floor.max(1e-12))
            .max(1e-12)
            .log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEECH: f32 = 3000.0; // int16 amplitude of the talker
    const ROOM: f32 = 30.0; // ... and of the room

    fn run(cycles: usize, speech_amp: f32, room_amp: f32) -> AdaptiveMixer {
        let speech = vec![speech_amp; 480];
        let pause = vec![room_amp; 480];
        let mut m = AdaptiveMixer::new(true);
        for _ in 0..cycles {
            m.update(&speech, 0.95);
            m.update(&pause, 0.02);
            m.update(&speech, 0.95);
            m.update(&pause, 0.02);
        }
        m
    }

    /// 40 dB local SNR: the mixer must learn the room from the pauses and then
    /// leave the talker alone.
    #[test]
    fn clean_speech_gets_bypassed() {
        let m = run(200, SPEECH, ROOM);
        assert!(
            m.wet < 0.1,
            "expected bypass on clean speech, got {}",
            m.wet
        );
        assert!(m.long_term_snr_db() > 35.0, "snr {}", m.long_term_snr_db());
    }

    /// 10 dB local SNR: this is the case denoising actually helps, so the model
    /// must stay fully engaged.
    #[test]
    fn noisy_speech_stays_wet() {
        let m = run(200, 95.0, ROOM);
        assert!(
            m.wet > 0.9,
            "expected full wet at 10 dB local SNR, got {}",
            m.wet
        );
    }

    /// A mic sitting in noise: never any speech above the floor.
    #[test]
    fn noise_floor_keeps_full_attenuation() {
        let hiss = vec![ROOM; 480];
        let mut m = AdaptiveMixer::new(true);
        for _ in 0..500 {
            m.update(&hiss, 0.01);
        }
        assert!(
            m.wet > 0.9,
            "expected full wet in the noise floor, got {}",
            m.wet
        );
    }

    /// Digital-silence pauses (what a clean synthetic reference looks like):
    /// the floor collapses, so the gate opens and the model is bypassed.
    #[test]
    fn digital_silence_pauses_open_the_gate() {
        let m = run(120, SPEECH, 0.0);
        assert!(m.wet < 0.1, "expected bypass, got {}", m.wet);
    }

    #[test]
    fn disabled_means_pure_model_output() {
        let mut m = AdaptiveMixer::new(false);
        assert_eq!(m.update(&vec![1.0f32; 480], 0.0), 1.0);
    }

    /// Drive the mixer with constant-amplitude speech frames so the learned
    /// long-term SNR settles at exactly `20*log10(speech_amp / first_amp)` dB:
    /// the first frame seeds `noise_floor == speech_level`, and every later
    /// frame is speech (vad above the threshold), so only `speech_level` moves.
    /// Returns the average wet ratio over the last `tail` frames.
    fn steady_state_wet(
        first_amp: f32,
        speech_amp: f32,
        frames: usize,
        tail: usize,
    ) -> (f64, f64, f64) {
        let first = vec![first_amp; 480];
        let rest = vec![speech_amp; 480];
        let mut m = AdaptiveMixer::new(true);
        m.update(&first, 0.95);
        let mut min = f64::MAX;
        let mut max = f64::MIN;
        let mut sum = 0.0;
        for i in 0..frames {
            let w = m.update(&rest, 0.95);
            if i >= frames - tail {
                min = min.min(w);
                max = max.max(w);
                sum += w;
            }
        }
        (sum / tail as f64, min, max)
    }

    /// Directional property 1: a constant input SNR must drive `wet` to a fixed
    /// point. With the hysteresis branches inverted, a constant SNR inside the
    /// band (gate_db +/- hysteresis_db) flips the gate every time wet crosses
    /// 0.5 and never settles -- a limit cycle.
    #[test]
    fn constant_snr_settles_without_limit_cycle() {
        let levels: [f32; 5] = [16.0, 20.0, 22.0, 24.0, 28.0]; // around gate_db +/- hysteresis_db
        for &db in &levels {
            let ratio = 10f32.powf(db / 20.0);
            let (_, min, max) = steady_state_wet(100.0, 100.0 * ratio, 1000, 100);
            let spread = max - min;
            assert!(
                spread < 1e-3,
                "{} dB SNR: wet limit-cycles, tail spread {}",
                db,
                spread
            );
        }
    }

    /// Directional property 2: the hysteresis must actually hold state -- a
    /// constant SNR inside the band settles *wet* from a wet start and *dry*
    /// from a dry start (entry bar gate_db - hysteresis_db is strictly below
    /// the exit bar gate_db + hysteresis_db).
    #[test]
    fn hysteresis_keeps_the_current_state_inside_the_band() {
        // 20 dB sits inside the band: wet state -> target 0.7, dry state -> 0.3.
        let first = vec![100.0f32; 480];
        let mid = vec![1000.0f32; 480]; // +20 dB over the floor
        let dry_drive = vec![10000.0f32; 480]; // +40 dB: drives wet -> 0

        // Wet start: a fresh mixer already sits at wet = 1.0.
        let (ss_wet, _, _) = {
            let mut m = AdaptiveMixer::new(true);
            m.update(&first, 0.95);
            let mut min = f64::MAX;
            let mut max = f64::MIN;
            let mut sum = 0.0;
            for i in 0..1000 {
                let w = m.update(&mid, 0.95);
                if i >= 900 {
                    min = min.min(w);
                    max = max.max(w);
                    sum += w;
                }
            }
            (sum / 100.0, min, max)
        };

        // Dry start: a clean high-SNR prefix collapses wet to ~0 first.
        let mut dry_start = AdaptiveMixer::new(true);
        dry_start.update(&first, 0.95);
        for _ in 0..800 {
            dry_start.update(&dry_drive, 0.95);
        }
        assert!(
            dry_start.wet < 0.05,
            "prefix must reach the dry state, wet={}",
            dry_start.wet
        );
        let mut ss_dry = 0.0;
        for i in 0..1000 {
            let w = dry_start.update(&mid, 0.95);
            if i >= 900 {
                ss_dry += w;
            }
        }
        ss_dry /= 100.0;

        assert!(
            ss_wet > 0.5,
            "wet start must stay wet inside the band, got {}",
            ss_wet
        );
        assert!(
            ss_dry < 0.5,
            "dry start must stay dry inside the band, got {}",
            ss_dry
        );
        assert!(
            ss_wet - ss_dry > 0.1,
            "steady states must differ (hysteresis width), wet={} dry={}",
            ss_wet,
            ss_dry
        );
    }
}

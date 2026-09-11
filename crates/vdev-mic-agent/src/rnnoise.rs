//! RNNoise, loaded at *runtime* through `libloading`.
//!
//! Why runtime loading instead of a link-time `-lrnnoise`:
//!   * the libraries we distribute are MinGW-built (`librnnoise-0.dll` +
//!     `librnnoise.dll.a`); MSVC's linker cannot consume the GNU import library,
//!     and asking users to own a `dlltool`-regenerated `.lib` is a support
//!     burden for zero benefit here;
//!   * the real product (vdev-mic-agent) has to degrade gracefully when the
//!     denoise backend is missing or fails to load -- a hard link-time
//!     dependency would make the whole agent unstartable;
//!   * the same `libloading` code path works on macOS (CoreAudio builds ship a
//!     `librnnoise.dylib`), so D3-D4 reuse this file unchanged.
//!
//! The DLL is resolved in order: explicit `--dll` path, then next to the
//! executable, then the loader's own search path.

use anyhow::{anyhow, bail, Context, Result};
use libloading::Library;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

pub const FRAME: usize = 480; // 10 ms @ 48 kHz, == rnnoise_get_frame_size()

type FnGetFrameSize = unsafe extern "C" fn() -> i32;
type FnGetSize = unsafe extern "C" fn() -> i32;
type FnCreate = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnProcessFrame = unsafe extern "C" fn(*mut c_void, *mut f32, *const f32) -> f32;
type FnDestroy = unsafe extern "C" fn(*mut c_void);

/// Loaded library + resolved entry points. Kept alive for as long as any
/// `Denoiser` created from it exists.
pub struct Engine {
    _lib: Library,
    create: FnCreate,
    process: FnProcessFrame,
    destroy: FnDestroy,
    pub frame_size: usize,
    pub state_bytes: usize,
    pub path: PathBuf,
}

impl Engine {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = explicit
            .map(|p| p.to_path_buf())
            .or_else(default_dll_candidates)
            .ok_or_else(|| anyhow!("could not find librnnoise; pass --dll <path>"))?;

        let lib = unsafe { Library::new(&path) }
            .with_context(|| format!("dlopen {}", path.display()))?;

        unsafe {
            let get_frame_size: FnGetFrameSize = *lib.get(b"rnnoise_get_frame_size\0")?;
            let get_size: FnGetSize = *lib.get(b"rnnoise_get_size\0")?;
            let create: FnCreate = *lib.get(b"rnnoise_create\0")?;
            let process: FnProcessFrame = *lib.get(b"rnnoise_process_frame\0")?;
            let destroy: FnDestroy = *lib.get(b"rnnoise_destroy\0")?;

            let frame_size = get_frame_size() as usize;
            if frame_size != FRAME {
                // Not fatal (a future RNNoise could change it) but worth knowing.
                eprintln!("note: rnnoise frame size is {frame_size}, expected {FRAME}");
            }

            Ok(Engine {
                _lib: lib,
                create,
                process,
                destroy,
                frame_size,
                state_bytes: get_size() as usize,
                path,
            })
        }
    }

    /// One denoiser == one microphone stream. 32 KB of RNN state.
    pub fn denoiser(&self) -> Result<Denoiser> {
        let st = unsafe { (self.create)(std::ptr::null_mut()) };
        if st.is_null() {
            bail!("rnnoise_create returned null");
        }
        Ok(Denoiser {
            st,
            process: self.process,
            destroy: self.destroy,
            frame_size: self.frame_size,
            out: vec![0.0f32; self.frame_size],
        })
    }
}

pub struct Denoiser {
    st: *mut c_void,
    process: FnProcessFrame,
    destroy: FnDestroy,
    frame_size: usize,
    out: Vec<f32>,
}

impl Denoiser {
    /// Push exactly one 10 ms frame through the model.
    /// Returns `(vad_probability, denoised_frame)`.
    ///
    /// `frame.len()` must be `frame_size`; the caller (the capture callback in
    /// the real agent) guarantees that by owning the frame splitter.
    pub fn process(&mut self, frame: &[f32]) -> (f32, &[f32]) {
        debug_assert_eq!(frame.len(), self.frame_size);
        let vad = unsafe { (self.process)(self.st, self.out.as_mut_ptr(), frame.as_ptr()) };
        (vad, &self.out)
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }
}

impl Drop for Denoiser {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.st) };
    }
}

const DLL_NAMES: [&str; 4] = [
    "librnnoise-0.dll",
    "rnnoise.dll",
    "librnnoise.dll",
    "librnnoise.dylib",
];

/// Vendored locations, relative to the executable's ancestors. `cargo run`
/// puts the binary in `<crate>/target/release/`, so walking up finds the
/// `third_party/` tree. Both layouts are covered: the demo archive, where the
/// tree sits next to the crate, and the vdev workspace, where the crate is at
/// `crates/vdev-mic-agent/` and the binary lands in the *repository's*
/// `target/`.
const VENDORED: [&str; 5] = [
    "third_party/pkgs/x/ucrt64/bin",
    "third_party/native",
    "third_party",
    "crates/vdev-mic-agent/third_party/native",
    "crates/vdev-mic-agent/third_party",
];

fn default_dll_candidates() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Ok(p) = std::env::var("RNNOISE_DLL") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors().skip(1).take(5) {
            dirs.push(anc.to_path_buf());
            for v in VENDORED {
                dirs.push(anc.join(v));
            }
        }
    }
    dirs.push(PathBuf::from("."));

    for d in dirs {
        for name in DLL_NAMES {
            let p = d.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

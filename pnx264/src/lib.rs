// Safe wrapper over the csrc/pnx264.c shim. See that file for why the FFI surface is a
// handful of primitives rather than bindings to x264_param_t.

pub mod linear;
pub mod parallel;
pub mod plan;
pub mod planner;
pub mod prefix;
pub mod y4m;

use std::ffi::{CStr, CString, c_char, c_float, c_int};
use std::os::raw::c_void;

#[repr(C)]
struct RawConfig {
    width: c_int,
    height: c_int,
    fps_num: c_int,
    fps_den: c_int,
    crf: c_float,
    threads: c_int,
    preset: *const c_char,
    tune: *const c_char,
    profile: *const c_char,
    level: *const c_char,
    x264_params: *const c_char,
    plan_only: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanEntry {
    pub frame_type: c_int,
    pub keyframe: c_int,
    pub is_idr: c_int,
    pub pts: i64,
}

unsafe extern "C" {
    fn pnx264_open(cfg: *const RawConfig, err: *mut *const c_char) -> *mut c_void;
    fn pnx264_headers(e: *mut c_void, out: *mut *const u8) -> c_int;
    fn pnx264_encode(
        e: *mut c_void,
        y: *const u8, u: *const u8, v: *const u8,
        sy: c_int, su: c_int, sv: c_int,
        pts: i64,
        out: *mut *const u8,
    ) -> c_int;
    fn pnx264_flush(e: *mut c_void, out: *mut *const u8) -> c_int;
    fn pnx264_plan_push(
        e: *mut c_void,
        y: *const u8, u: *const u8, v: *const u8,
        sy: c_int, su: c_int, sv: c_int,
        pts: i64,
        out: *mut PlanEntry,
    ) -> c_int;
    fn pnx264_plan_flush(e: *mut c_void, out: *mut PlanEntry) -> c_int;
    fn pnx264_close(e: *mut c_void);
}

// Mirrors the CPU_* entries in src/lib/mpeg/preset.rs. `x264_params` carries the same string
// the ffmpeg path passes as -x264-params, so a preset can be reproduced exactly.
#[derive(Clone, Debug)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub crf: f32,
    pub threads: u32,
    pub preset: Option<String>,
    pub tune: Option<String>,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub x264_params: Option<String>,
    // Lookahead only, no macroblock coding. Needs the Pandora x264 fork.
    pub plan_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps_num: 24,
            fps_den: 1,
            crf: 18.0,
            threads: 1,
            preset: None,
            tune: None,
            profile: None,
            level: None,
            x264_params: None,
            plan_only: false,
        }
    }
}

pub struct Encoder {
    raw: *mut c_void,
    // Kept alive only so the CStrings outlive the pnx264_open call that borrows them.
    _strings: Vec<CString>,
}

// The encoder owns its x264_t and is never shared; moving it between threads is what the
// chunk workers need. It is deliberately not Sync.
unsafe impl Send for Encoder {}

fn cstr(v: &Option<String>, keep: &mut Vec<CString>) -> *const c_char {
    match v {
        Some(s) => {
            let c = CString::new(s.as_str()).expect("preset strings never contain NUL");
            let p = c.as_ptr();
            keep.push(c);
            p
        }
        None => std::ptr::null(),
    }
}

impl Encoder {
    pub fn open(cfg: &Config) -> Result<Self, String> {
        let mut keep = Vec::new();
        let raw_cfg = RawConfig {
            width: cfg.width as c_int,
            height: cfg.height as c_int,
            fps_num: cfg.fps_num as c_int,
            fps_den: cfg.fps_den as c_int,
            crf: cfg.crf,
            threads: cfg.threads as c_int,
            preset: cstr(&cfg.preset, &mut keep),
            tune: cstr(&cfg.tune, &mut keep),
            profile: cstr(&cfg.profile, &mut keep),
            level: cstr(&cfg.level, &mut keep),
            x264_params: cstr(&cfg.x264_params, &mut keep),
            plan_only: cfg.plan_only as c_int,
        };
        let mut err: *const c_char = std::ptr::null();
        let raw = unsafe { pnx264_open(&raw_cfg, &mut err) };
        if raw.is_null() {
            let msg = if err.is_null() {
                "x264 open failed".to_string()
            } else {
                unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned()
            };
            return Err(msg);
        }
        Ok(Self { raw, _strings: keep })
    }

    pub fn headers(&mut self) -> Result<&[u8], String> {
        let mut out: *const u8 = std::ptr::null();
        let n = unsafe { pnx264_headers(self.raw, &mut out) };
        Self::slice(n, out, "x264_encoder_headers failed")
    }

    // Planes are yuv420p. Returns the NAL bytes produced by this call, which may be empty
    // while x264 has the frame buffered behind its lookahead.
    pub fn encode(
        &mut self,
        y: &[u8], u: &[u8], v: &[u8],
        stride_y: usize, stride_u: usize, stride_v: usize,
        pts: i64,
    ) -> Result<&[u8], String> {
        let mut out: *const u8 = std::ptr::null();
        let n = unsafe {
            pnx264_encode(
                self.raw,
                y.as_ptr(), u.as_ptr(), v.as_ptr(),
                stride_y as c_int, stride_u as c_int, stride_v as c_int,
                pts, &mut out,
            )
        };
        Self::slice(n, out, "x264_encoder_encode failed")
    }

    // Drains one buffered frame. An empty slice means the encoder has nothing left.
    pub fn flush(&mut self) -> Result<&[u8], String> {
        let mut out: *const u8 = std::ptr::null();
        let n = unsafe { pnx264_flush(self.raw, &mut out) };
        Self::slice(n, out, "x264 flush failed")
    }

    // Plan-only counterparts. Ok(None) means the frame is still inside the lookahead and no
    // decision has been made for it yet.
    pub fn plan_push(
        &mut self,
        y: &[u8], u: &[u8], v: &[u8],
        stride_y: usize, stride_u: usize, stride_v: usize,
        pts: i64,
    ) -> Result<Option<PlanEntry>, String> {
        let mut out = PlanEntry::default();
        let n = unsafe {
            pnx264_plan_push(
                self.raw,
                y.as_ptr(), u.as_ptr(), v.as_ptr(),
                stride_y as c_int, stride_u as c_int, stride_v as c_int,
                pts, &mut out,
            )
        };
        match n {
            1 => Ok(Some(out)),
            0 => Ok(None),
            _ => Err("x264 plan push failed".to_string()),
        }
    }

    pub fn plan_flush(&mut self) -> Result<Option<PlanEntry>, String> {
        let mut out = PlanEntry::default();
        match unsafe { pnx264_plan_flush(self.raw, &mut out) } {
            1 => Ok(Some(out)),
            0 => Ok(None),
            _ => Err("x264 plan flush failed".to_string()),
        }
    }

    fn slice<'a>(n: c_int, out: *const u8, err: &str) -> Result<&'a [u8], String> {
        if n < 0 {
            return Err(err.to_string());
        }
        if n == 0 || out.is_null() {
            return Ok(&[]);
        }
        Ok(unsafe { std::slice::from_raw_parts(out, n as usize) })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { pnx264_close(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // x264_encoder_headers() emits SPS/PPS plus the SEI carrying x264's full option string,
    // without encoding a frame, so these assert on settings rather than on output bytes.
    fn options_of(cfg: &Config) -> String {
        let mut enc = Encoder::open(cfg).expect("open encoder");
        let hdr = enc.headers().expect("headers").to_vec();
        String::from_utf8_lossy(&hdr).into_owned()
    }

    fn hd_veryslow(level: Option<&str>) -> Config {
        Config {
            width: 1920,
            height: 1080,
            fps_num: 24,
            fps_den: 1,
            crf: 18.0,
            threads: 1,
            preset: Some("veryslow".into()),
            profile: Some("high".into()),
            level: level.map(str::to_string),
            ..Default::default()
        }
    }

    // The clamp libx264 does NOT do for us. Both the x264 CLI and ffmpeg reduce the reference
    // count to the target level's DPB limit, so a shim that skips it silently disagrees with
    // production output. At 1080p, level 4.1 allows 4 refs (MaxDpbMbs 32768 / 8040 MBs per
    // frame) while `veryslow` asks for 16.
    #[test]
    fn refs_are_clamped_to_the_level_dpb_limit() {
        let opts = options_of(&hd_veryslow(Some("4.1")));
        assert!(opts.contains(" ref=4 "), "expected ref=4 at 1080p level 4.1, got: {opts}");
    }

    // Guards the other direction: the clamp must not fire when no level is requested, or it
    // would be silently degrading quality rather than enforcing conformance.
    #[test]
    fn refs_are_untouched_without_a_level() {
        let opts = options_of(&hd_veryslow(None));
        assert!(opts.contains(" ref=16 "), "expected veryslow's ref=16, got: {opts}");
    }

    // The presets in src/lib/mpeg/preset.rs pass their tuning through -x264-params; the shim
    // has to route that to x264_param_parse the same way.
    #[test]
    fn x264_params_reach_the_encoder() {
        let mut cfg = hd_veryslow(Some("4.1"));
        cfg.x264_params = Some("aq-mode=3:aq-strength=1.1".into());
        let opts = options_of(&cfg);
        assert!(opts.contains("aq=3:1.10"), "expected aq=3:1.10, got: {opts}");
    }

    #[test]
    fn a_bad_preset_is_an_error_not_a_panic() {
        let mut cfg = hd_veryslow(None);
        cfg.preset = Some("definitely-not-a-preset".into());
        assert!(Encoder::open(&cfg).is_err());
    }
}

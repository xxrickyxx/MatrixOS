// ── Hardware-accelerated primitives ──────────────────────────────────────────

/// Runtime flag set during boot — true if the CPU supports AVX2 + FMA and
/// the OS has enabled the AVX state-save area in XCR0.  Read by mat_mul_vec
/// / dot to pick the 8-wide path.  Set exactly once from main::_start.
static mut AVX2_AVAILABLE: bool = false;

#[inline(always)]
pub fn avx2_available() -> bool { unsafe { AVX2_AVAILABLE } }

#[inline(always)]
pub fn set_avx2_available(v: bool) { unsafe { AVX2_AVAILABLE = v; } }

/// Probe via CPUID whether the CPU supports AVX2 + FMA + OSXSAVE.
/// Returns true only if the CPU has every prerequisite; the caller is
/// still responsible for enabling AVX state in XCR0 (see math::enable_avx).
pub fn cpu_supports_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // CPUID leaf 1: feature flags (ECX bit 28 = AVX, bit 27 = OSXSAVE,
        // bit 12 = FMA)
        let leaf1_ecx: u32;
        let leaf1_eax: u32;
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {tmp_ebx:e}, ebx",
            "pop rbx",
            out("eax") leaf1_eax,
            out("ecx") leaf1_ecx,
            out("edx") _,
            tmp_ebx = out(reg) _,
        );
        let _ = leaf1_eax;
        let has_osxsave = (leaf1_ecx & (1 << 27)) != 0;
        let has_avx     = (leaf1_ecx & (1 << 28)) != 0;
        let has_fma     = (leaf1_ecx & (1 << 12)) != 0;

        // CPUID leaf 7, sub-leaf 0: extended features (EBX bit 5 = AVX2)
        let mut leaf7_ebx: u32;
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            ebx_out = out(reg) leaf7_ebx,
        );
        let has_avx2 = (leaf7_ebx & (1 << 5)) != 0;

        has_osxsave && has_avx && has_fma && has_avx2
    }
    #[cfg(not(target_arch = "x86_64"))]
    { false }
}

/// Tell the CPU we will use AVX (sets OSXSAVE + XCR0.YMM bits).
/// Must be called from a context where CR4.OSXSAVE can be modified.
/// SAFETY: only call once, after `cpu_supports_avx2()` returned true,
/// and before any AVX instruction executes.
pub unsafe fn enable_avx() {
    // Set CR4.OSXSAVE (bit 18)
    core::arch::asm!(
        "mov rax, cr4",
        "or  eax, 1 << 18",
        "mov cr4, rax",
        out("rax") _,
        options(nostack)
    );
    // Set XCR0 = X87 | SSE | YMM (bits 0,1,2)
    core::arch::asm!(
        "xor ecx, ecx",
        "mov eax, 7",
        "xor edx, edx",
        "xsetbv",
        out("eax") _,
        out("ecx") _,
        out("edx") _,
        options(nostack)
    );
}

/// CPUID leaf 0: 12-byte vendor string (e.g. "GenuineIntel", "AuthenticAMD").
/// Writes 12 bytes into `out`.
pub fn cpu_vendor(out: &mut [u8; 12]) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let ebx: u32; let ecx: u32; let edx: u32;
        core::arch::asm!(
            "push rbx",
            "mov eax, 0",
            "cpuid",
            "mov {ebx_o:e}, ebx",
            "pop rbx",
            out("eax") _,
            out("ecx") ecx,
            out("edx") edx,
            ebx_o = out(reg) ebx,
        );
        out[0..4].copy_from_slice(&ebx.to_le_bytes());
        out[4..8].copy_from_slice(&edx.to_le_bytes());
        out[8..12].copy_from_slice(&ecx.to_le_bytes());
    }
    #[cfg(not(target_arch = "x86_64"))]
    { let _ = out; }
}

/// CPUID leaves 0x80000002..4: 48-byte processor brand string.
/// Writes up to 48 bytes into `out`; returns the actual length used.
pub fn cpu_brand(out: &mut [u8; 48]) -> usize {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // First confirm leaves 80000002..4 exist.
        let max_ext: u32;
        core::arch::asm!(
            "mov eax, 0x80000000",
            "cpuid",
            out("eax") max_ext,
            out("ecx") _,
            out("edx") _,
        );
        if max_ext < 0x8000_0004 {
            for b in out.iter_mut() { *b = 0; }
            return 0;
        }
        for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004].iter().enumerate() {
            let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
            core::arch::asm!(
                "push rbx",
                "cpuid",
                "mov {ebx_o:e}, ebx",
                "pop rbx",
                in("eax") *leaf,
                out("ecx") ecx,
                out("edx") edx,
                lateout("eax") eax,
                ebx_o = out(reg) ebx,
            );
            let base = i * 16;
            out[base..base + 4].copy_from_slice(&eax.to_le_bytes());
            out[base + 4..base + 8].copy_from_slice(&ebx.to_le_bytes());
            out[base + 8..base + 12].copy_from_slice(&ecx.to_le_bytes());
            out[base + 12..base + 16].copy_from_slice(&edx.to_le_bytes());
        }
        // Strip trailing spaces / zeros for length.
        let mut len = out.len();
        while len > 0 && (out[len - 1] == 0 || out[len - 1] == b' ') { len -= 1; }
        len
    }
    #[cfg(not(target_arch = "x86_64"))]
    { let _ = out; 0 }
}

/// CPUID leaf 1 EBX bits 16..24 = max addressable logical processors per
/// package (a *rough* core/thread count proxy; not authoritative under
/// hyperthreading but good enough for a sidebar).
pub fn cpu_logical_count() -> u32 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let ebx_v: u32;
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {ebx_o:e}, ebx",
            "pop rbx",
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            ebx_o = out(reg) ebx_v,
        );
        (ebx_v >> 16) & 0xFF
    }
    #[cfg(not(target_arch = "x86_64"))]
    { 1 }
}

/// Cached TSC frequency in Hz, populated by calibrate_tsc_via_pit().
/// Default 3 GHz so /uptime is roughly right even without calibration.
static mut TSC_HZ: u64 = 3_000_000_000;

#[inline(always)]
pub fn tsc_hz() -> u64 { unsafe { TSC_HZ } }

/// Calibrate the TSC against the 8254 PIT.  The PIT runs at 1.193182 MHz
/// regardless of CPU speed, so we measure how many TSC ticks elapse during
/// a fixed PIT countdown and convert to Hz.
///
/// Programs PIT channel 2 (the speaker channel, safe to use without
/// interrupting timers) to count down ~50 ms at 1.193182 MHz.  Reads TSC
/// before and after; (delta_tsc * 1_193_182) / pit_ticks ≈ TSC_HZ.
pub unsafe fn calibrate_tsc_via_pit() {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::asm;
        // Enable PIT channel 2 gate via port 0x61, bit 0 = 1, bit 1 = 0 (speaker mute)
        let prev: u8;
        asm!("in al, 0x61", out("al") prev, options(nostack, nomem, preserves_flags));
        let new = (prev & 0xFC) | 0x01;
        asm!("out 0x61, al", in("al") new, options(nostack, nomem, preserves_flags));

        // Program channel 2, mode 0 (interrupt on terminal count), binary, LSB then MSB
        asm!("out 0x43, al", in("al") 0xB0u8, options(nostack, nomem, preserves_flags));
        // Count value: 1193182 Hz * 0.05 s ≈ 59659.  We'll use a lower number to
        // avoid potential 16-bit overflow on slower machines.  Use 11932 = ~10 ms.
        let count: u16 = 11932;
        asm!("out 0x42, al", in("al") (count & 0xFF) as u8, options(nostack, nomem, preserves_flags));
        asm!("out 0x42, al", in("al") (count >> 8)   as u8, options(nostack, nomem, preserves_flags));

        let tsc_start = rdtsc();

        // Wait for OUT2 (port 0x61 bit 5) to go high — terminal count reached.
        loop {
            let s: u8;
            asm!("in al, 0x61", out("al") s, options(nostack, nomem, preserves_flags));
            if s & 0x20 != 0 { break; }
        }

        let tsc_end = rdtsc();

        // Restore previous port 0x61 state.
        asm!("out 0x61, al", in("al") prev, options(nostack, nomem, preserves_flags));

        let delta_tsc = tsc_end.wrapping_sub(tsc_start);
        // count PIT ticks elapsed at 1_193_182 Hz
        // ticks / 1193182 = seconds elapsed
        // tsc_hz = delta_tsc / seconds = delta_tsc * 1193182 / count
        let hz = delta_tsc.saturating_mul(1_193_182u64) / (count as u64);
        // Sanity: clamp to plausible range 500MHz .. 8GHz
        if (500_000_000..=8_000_000_000).contains(&hz) {
            TSC_HZ = hz;
        }
    }
}

/// Read the x86_64 Time Stamp Counter (TSC).
/// Used for rough timing; calibration-free (caller divides by ~CPU_HZ).
#[inline(always)]
pub fn rdtsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
        ((hi as u64) << 32) | (lo as u64)
    }
    #[cfg(not(target_arch = "x86_64"))]
    { 0 }
}

/// SSE3 horizontal-add helper: returns a[0]+a[1]+a[2]+a[3].
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse3")]
#[inline]
unsafe fn hsum_sse(v: core::arch::x86_64::__m128) -> f32 {
    use core::arch::x86_64::*;
    let h1 = _mm_hadd_ps(v, v);
    let h2 = _mm_hadd_ps(h1, h1);
    _mm_cvtss_f32(h2)
}

/// SSE dot product of two equal-length f32 slices.
/// Falls back to scalar for the remainder (n % 4 ≠ 0) elements.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse3")]
unsafe fn dot_sse(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::x86_64::*;
    let n      = a.len().min(b.len());
    let chunks = n / 4;
    let mut acc = _mm_setzero_ps();
    let ap = a.as_ptr();
    let bp = b.as_ptr();
    for i in 0..chunks {
        let av = _mm_loadu_ps(ap.add(i * 4));
        let bv = _mm_loadu_ps(bp.add(i * 4));
        // FMA not assumed; plain mul+add is universal
        acc = _mm_add_ps(acc, _mm_mul_ps(av, bv));
    }
    let mut sum = hsum_sse(acc);
    for i in (chunks * 4)..n {
        sum += *ap.add(i) * *bp.add(i);
    }
    sum
}

/// SSE matrix-vector multiply: out[i] = Σ vec[j]*mat[i*cols+j]
/// Each row dot is vectorised with SSE3 (4-wide f32).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse3")]
unsafe fn mat_mul_vec_sse(out: &mut [f32], vec: &[f32], mat: &[f32]) {
    let rows = out.len();
    let cols = vec.len();
    for i in 0..rows {
        let row = core::slice::from_raw_parts(mat.as_ptr().add(i * cols), cols);
        out[i] = dot_sse(vec, row);
    }
}

// ── AVX2 + FMA versions (8-wide, ~2× the throughput of SSE3) ────────────────
//
// Only callable on CPUs that pass cpu_supports_avx2() AND after enable_avx()
// has set OSXSAVE + XCR0.  The runtime mat_mul_vec dispatcher checks the
// global AVX2_AVAILABLE flag.

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,avx2,fma")]
#[inline]
unsafe fn hsum_avx(v: core::arch::x86_64::__m256) -> f32 {
    use core::arch::x86_64::*;
    // Split into two 128-bit lanes and sum.
    let lo = _mm256_castps256_ps128(v);
    let hi = _mm256_extractf128_ps(v, 1);
    let s  = _mm_add_ps(lo, hi);
    // Horizontal-add the 4 lanes.
    let s2 = _mm_hadd_ps(s, s);
    let s3 = _mm_hadd_ps(s2, s2);
    _mm_cvtss_f32(s3)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,avx2,fma")]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::x86_64::*;
    let n      = a.len().min(b.len());
    let chunks = n / 8;
    let mut acc = _mm256_setzero_ps();
    let ap = a.as_ptr();
    let bp = b.as_ptr();
    for i in 0..chunks {
        let av = _mm256_loadu_ps(ap.add(i * 8));
        let bv = _mm256_loadu_ps(bp.add(i * 8));
        // Fused-multiply-add: acc += av * bv (one micro-op on Haswell+)
        acc = _mm256_fmadd_ps(av, bv, acc);
    }
    let mut sum = hsum_avx(acc);
    for i in (chunks * 8)..n {
        sum += *ap.add(i) * *bp.add(i);
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,avx2,fma")]
unsafe fn mat_mul_vec_avx2(out: &mut [f32], vec: &[f32], mat: &[f32]) {
    let rows = out.len();
    let cols = vec.len();
    for i in 0..rows {
        let row = core::slice::from_raw_parts(mat.as_ptr().add(i * cols), cols);
        out[i] = dot_avx2(vec, row);
    }
}

/// Matrix-vector multiply: out[i] = dot(vec, mat_row[i])
/// Runtime-dispatches between AVX2+FMA (8-wide), SSE3 (4-wide), and scalar.
pub fn mat_mul_vec(out: &mut [f32], vec: &[f32], mat: &[f32]) {
    #[cfg(target_arch = "x86_64")]
    {
        if avx2_available() {
            unsafe { mat_mul_vec_avx2(out, vec, mat); }
        } else {
            unsafe { mat_mul_vec_sse(out, vec, mat); }
        }
        return;
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let rows = out.len();
        let cols = vec.len();
        for i in 0..rows {
            let mut s = 0.0f32;
            let off   = i * cols;
            for j in 0..cols { s += vec[j] * mat[off + j]; }
            out[i] = s;
        }
    }
}

/// RMSNorm (out = x * weight / rms(x))
pub fn rms_norm(out: &mut [f32], x: &[f32], weight: &[f32]) {
    let n  = x.len();
    let mut ss = 1e-5f32;
    for v in x { ss += v * v; }
    ss /= n as f32;
    let inv = fast_inv_sqrt(ss);
    for i in 0..n {
        out[i] = weight[i] * (inv * x[i]);
    }
}

#[inline(always)]
pub fn fast_inv_sqrt(x: f32) -> f32 {
    // Quake III fast inverse sqrt — good enough for RMSNorm
    let mut i = x.to_bits();
    i = 0x5f3759df - (i >> 1);
    let mut y = f32::from_bits(i);
    y = y * (1.5 - 0.5 * x * y * y);
    y
}

/// Softmax in-place (numerically stable: subtract max first).
pub fn softmax(x: &mut [f32]) {
    let n = x.len();
    if n == 0 { return; }
    let mut max = x[0];
    for &v in &x[1..] { if v > max { max = v; } }
    let mut sum = 0.0f32;
    for v in x.iter_mut() { *v = fast_exp(*v - max); sum += *v; }
    // Guard: if sum is zero or negative (shouldn't happen with clamped fast_exp,
    // but be safe), fall back to uniform distribution.
    let inv = if sum > 1e-30 { 1.0 / sum } else { 1.0 / n as f32 };
    for v in x.iter_mut() { *v *= inv; }
}

/// Fast approximation of e^x suitable for softmax.
#[inline(always)]
pub fn fast_exp(x: f32) -> f32 {
    // CRITICAL: clamp before the integer cast.  For x < -87, the biased exponent
    // (ti + 127) goes negative; casting to u32 wraps around producing a huge
    // negative float (e.g. -5e33) instead of ≈0.  That poisons the softmax sum
    // (makes it negative → normalization skipped → -inf attention weights → NaN).
    if x < -87.0 { return 0.0; }
    if x >  87.0 { return 3.402_823_5e38_f32; }   // near f32::MAX, won't overflow
    const LN2_INV: f32 = 1.442695_f32;
    let t = x * LN2_INV;
    let ti = t as i32;
    let tf = t - ti as f32;
    let p = 1.0 + tf * (0.6931472 + tf * (0.2402265 + tf * (0.0555041 + tf * 0.0096181)));
    let exp_bits = ((ti + 127) as u32) << 23;
    let base = f32::from_bits(exp_bits);
    base * p
}

/// SiLU(x) = x * sigmoid(x) = x / (1 + e^-x)
#[inline(always)]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + fast_exp(-x))
}

/// Dot product — AVX2+FMA when available, SSE3 fallback, scalar elsewhere.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if avx2_available() {
            return unsafe { dot_avx2(a, b) };
        }
        return unsafe { dot_sse(a, b) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let n = a.len().min(b.len());
        let mut s = 0.0f32;
        for i in 0..n { s += a[i] * b[i]; }
        s
    }
}

/// Copy src → dst.
pub fn vcopy(dst: &mut [f32], src: &[f32]) {
    let n = dst.len().min(src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

/// Elementwise add: dst += src
pub fn vadd(dst: &mut [f32], src: &[f32]) {
    let n = dst.len().min(src.len());
    for i in 0..n { dst[i] += src[i]; }
}

/// Apply RoPE rotary position encoding to query and key vectors in-place.
/// head_dim must be even.  freq_base is e.g. 1_000_000 for Qwen2.
pub fn rope_apply(x: &mut [f32], pos: usize, head_dim: usize, freq_base: f32) {
    let half = head_dim / 2;
    let pos_f = pos as f32;
    for i in 0..half {
        // θ_i = pos / freq_base^(2i/d)
        let exp    = (2 * i) as f32 / head_dim as f32;
        let inv_f  = fast_pow(freq_base, exp);
        let theta  = pos_f / inv_f;
        let (sin_t, cos_t) = fast_sin_cos(theta);
        let x0 = x[i];
        let x1 = x[i + half];
        x[i]        = x0 * cos_t - x1 * sin_t;
        x[i + half] = x0 * sin_t + x1 * cos_t;
    }
}

/// Integer-power float helper used by rope.
#[inline(always)]
pub fn fast_pow(base: f32, exp: f32) -> f32 {
    // b^x = e^(x * ln(b)) = 2^(x * log2(b))
    const LOG2E: f32 = 1.442695;
    // ln(b) = log2(b) / log2(e)
    let log2b = fast_log2(base);
    let t     = exp * log2b;
    // 2^t via fast_exp
    fast_exp(t / LOG2E)
}

#[inline(always)]
pub fn fast_log2(x: f32) -> f32 {
    let bits = x.to_bits();
    let exp  = ((bits >> 23) & 0xFF) as i32 - 127;
    let mant = f32::from_bits((bits & 0x007FFFFF) | 0x3F800000); // [1,2)
    // log2(mant) polynomial in [1,2)
    let m = mant - 1.0;
    let p = m * (1.4427 - m * 0.7214 * 0.5);
    exp as f32 + p
}

/// Approximate sin/cos via Taylor series for small |x|, then range-reduce.
pub fn fast_sin_cos(x: f32) -> (f32, f32) {
    // Reduce x to [-π, π]
    const PI:   f32 = 3.14159265;
    const PI2:  f32 = 6.28318530;
    let mut t = x;
    // Coarse range reduction
    while t >  PI { t -= PI2; }
    while t < -PI { t += PI2; }
    // Polynomial approximation (Bhaskara I / minimax)
    let t2 = t * t;
    let s  = t * (1.0 - t2 * (0.16667 - t2 * (0.00833 - t2 * 0.000198)));
    let c  = 1.0 - t2 * (0.5 - t2 * (0.04167 - t2 * 0.001389));
    (s, c)
}

/// Argmax of a slice — returns index of largest element.
pub fn argmax(x: &[f32]) -> usize {
    let mut best_i = 0;
    let mut best_v = x[0];
    for (i, &v) in x.iter().enumerate() {
        if v > best_v { best_v = v; best_i = i; }
    }
    best_i
}

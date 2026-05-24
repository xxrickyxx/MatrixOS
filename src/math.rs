/// Matrix-vector multiply: out[i] = dot(vec, mat_row[i])
pub fn mat_mul_vec(out: &mut [f32], vec: &[f32], mat: &[f32]) {
    let rows = out.len();
    let cols = vec.len();
    for i in 0..rows {
        let mut s = 0.0f32;
        let off   = i * cols;
        for j in 0..cols { s += vec[j] * mat[off + j]; }
        out[i] = s;
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
    let n   = x.len();
    if n == 0 { return; }
    let mut max = x[0];
    for &v in &x[1..] { if v > max { max = v; } }
    let mut sum = 0.0f32;
    for v in x.iter_mut() { *v = fast_exp(*v - max); sum += *v; }
    if sum > 0.0 {
        for v in x.iter_mut() { *v /= sum; }
    }
}

/// Fast approximation of e^x suitable for softmax.
#[inline(always)]
pub fn fast_exp(x: f32) -> f32 {
    // Polynomial approximation accurate to ~0.1% for |x| < 88
    // Using: e^x ≈ 2^(x * log2(e)) via bit manipulation
    const LN2_INV: f32 = 1.442695_f32;
    let t = x * LN2_INV;
    // Split into integer and fractional parts
    let ti = t as i32;
    let tf = t - ti as f32;
    // Polynomial for 2^tf (Horner form, [0,1])
    let p = 1.0 + tf * (0.6931472 + tf * (0.2402265 + tf * (0.0555041 + tf * 0.0096181)));
    // Assemble IEEE 754 exponent
    let exp_bits = ((ti + 127) as u32) << 23;
    let base = f32::from_bits(exp_bits);
    base * p
}

/// SiLU(x) = x * sigmoid(x) = x / (1 + e^-x)
#[inline(always)]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + fast_exp(-x))
}

/// Dot product.
#[inline(always)]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut s = 0.0f32;
    for i in 0..n { s += a[i] * b[i]; }
    s
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

pub fn mat_mul_vec(out: &mut [f32], vec: &[f32], mat: &[f32]) {
    let rows = out.len();
    let cols = vec.len();
    
    for i in 0..rows {
        let mut sum = 0.0;
        let row_offset = i * cols;
        for j in 0..cols {
            sum += vec[j] * mat[row_offset + j];
        }
        out[i] = sum;
    }
}

pub fn rms_norm(out: &mut [f32], x: &[f32], weight: &[f32]) {
    let mut ss = 0.0;
    for i in 0..x.len() {
        ss += x[i] * x[i];
    }
    ss /= x.len() as f32;
    ss += 1e-5;
    
    // Fast inverse square root (Quake style) per no_std
    let mut i = ss.to_bits();
    i = 0x5f3759df - (i >> 1);
    let mut y = f32::from_bits(i);
    y = y * (1.5 - 0.5 * ss * y * y);
    
    for i in 0..x.len() {
        out[i] = weight[i] * (y * x[i]);
    }
}

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
use crate::gguf::{Gguf, GgufValue, ggml_type_size};
use crate::quant::dequant_row;
use crate::math::{rms_norm, softmax, silu, dot, rope_apply, fast_inv_sqrt, argmax};

// ── Model hyperparameters ─────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct ModelConfig {
    pub d_model:       usize,
    pub n_layers:      usize,
    pub n_heads:       usize,
    pub n_kv_heads:    usize,
    pub head_dim:      usize,
    pub ffn_dim:       usize,
    pub vocab_size:    usize,
    pub rope_freq:     f32,
    pub bos:           u32,
    pub eos:           u32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            d_model:    3584,
            n_layers:   28,
            n_heads:    28,
            n_kv_heads: 4,
            head_dim:   128,
            ffn_dim:    18944,
            vocab_size: 152064,
            rope_freq:  1_000_000.0,
            bos:        151643,
            eos:        151645,
        }
    }
}

pub fn load_config_from_gguf(gguf: &Gguf) -> ModelConfig {
    let mut cfg = ModelConfig::default();

    // ── Detect architecture prefix (qwen2, llama, gemma, …) ───────────────
    // GGUF metadata keys are namespaced by architecture, e.g. for Qwen2.5
    // the keys are `qwen2.embedding_length` rather than `llama.embedding_length`.
    // Without this, every architecture-specific key fails to resolve and the
    // model silently falls back to the 7B `ModelConfig::default()` values —
    // which is a disaster for any model whose real shape differs.
    let arch_name: &str = if let Some(GgufValue::String(s)) = gguf.find_metadata("general.architecture") {
        s
    } else {
        "llama"
    };

    // small stack buffer to compose "<arch>.<suffix>" strings without alloc
    let mut keybuf = [0u8; 96];
    fn make_key<'a>(arch: &str, suffix: &str, buf: &'a mut [u8; 96]) -> &'a str {
        let mut i = 0usize;
        for &b in arch.as_bytes()    { if i < 95 { buf[i] = b; i += 1; } }
        if i < 95 { buf[i] = b'.'; i += 1; }
        for &b in suffix.as_bytes()  { if i < 95 { buf[i] = b; i += 1; } }
        core::str::from_utf8(&buf[..i]).unwrap_or("")
    }

    // Lookup: try "<arch>.<suffix>" first, then fall back to "llama.<suffix>"
    let lookup = |suffix: &str, buf: &mut [u8; 96]| -> Option<GgufValue> {
        let k1 = make_key(arch_name, suffix, buf);
        if let Some(v) = gguf.find_metadata(k1) { return Some(v); }
        if arch_name != "llama" {
            let mut buf2 = [0u8; 96];
            let k2 = make_key("llama", suffix, &mut buf2);
            return gguf.find_metadata(k2);
        }
        None
    };

    macro_rules! read_u32_x {
        ($suffix:expr, $field:expr) => {
            if let Some(v) = lookup($suffix, &mut keybuf) {
                match v {
                    GgufValue::Uint32(n) => $field = n as usize,
                    GgufValue::Int32(n)  => $field = n as usize,
                    GgufValue::Uint64(n) => $field = n as usize,
                    _ => {}
                }
            }
        }
    }
    macro_rules! read_f32_x {
        ($suffix:expr, $field:expr) => {
            if let Some(v) = lookup($suffix, &mut keybuf) {
                if let GgufValue::Float32(f) = v { $field = f; }
            }
        }
    }

    read_u32_x!("embedding_length",        cfg.d_model);
    read_u32_x!("block_count",             cfg.n_layers);
    read_u32_x!("attention.head_count",    cfg.n_heads);
    read_u32_x!("attention.head_count_kv", cfg.n_kv_heads);
    read_u32_x!("feed_forward_length",     cfg.ffn_dim);
    read_u32_x!("vocab_size",              cfg.vocab_size);
    read_f32_x!("rope.freq_base",          cfg.rope_freq);

    if let Some(GgufValue::Uint32(v)) = gguf.find_metadata("tokenizer.ggml.bos_token_id") {
        cfg.bos = v;
    }
    if let Some(GgufValue::Uint32(v)) = gguf.find_metadata("tokenizer.ggml.eos_token_id") {
        cfg.eos = v;
    }

    if cfg.n_heads > 0 { cfg.head_dim = cfg.d_model / cfg.n_heads; }
    cfg
}

// ── KV-cache ──────────────────────────────────────────────────────────────────

pub struct KVCache {
    pub k:          Vec<f32>,
    pub v:          Vec<f32>,
    pub n_layers:   usize,
    pub n_kv_heads: usize,
    pub head_dim:   usize,
    pub max_seq:    usize,
}

impl KVCache {
    pub fn new(n_layers: usize, n_kv_heads: usize, head_dim: usize, max_seq: usize) -> Self {
        let sz = n_layers * n_kv_heads * max_seq * head_dim;
        Self { k: vec![0f32; sz], v: vec![0f32; sz], n_layers, n_kv_heads, head_dim, max_seq }
    }

    #[inline(always)]
    fn offset(&self, layer: usize, head: usize, pos: usize) -> usize {
        layer * (self.n_kv_heads * self.max_seq * self.head_dim)
        + head * (self.max_seq * self.head_dim)
        + pos  * self.head_dim
    }

    pub fn k_slice(&self, layer: usize, head: usize, pos: usize) -> &[f32] {
        let o = self.offset(layer, head, pos); &self.k[o..o + self.head_dim]
    }
    pub fn v_slice(&self, layer: usize, head: usize, pos: usize) -> &[f32] {
        let o = self.offset(layer, head, pos); &self.v[o..o + self.head_dim]
    }
    pub fn k_slice_mut(&mut self, layer: usize, head: usize, pos: usize) -> &mut [f32] {
        let o = self.offset(layer, head, pos); &mut self.k[o..o + self.head_dim]
    }
    pub fn v_slice_mut(&mut self, layer: usize, head: usize, pos: usize) -> &mut [f32] {
        let o = self.offset(layer, head, pos); &mut self.v[o..o + self.head_dim]
    }
}

// ── Scratch buffers ───────────────────────────────────────────────────────────

pub struct ForwardScratch {
    pub x:           Vec<f32>,
    pub xb:          Vec<f32>,
    pub xb2:         Vec<f32>,
    pub q:           Vec<f32>,
    pub k_proj:      Vec<f32>,
    pub v_proj:      Vec<f32>,
    pub attn_out:    Vec<f32>,
    pub attn_scores: Vec<f32>,
    pub ffn_gate:    Vec<f32>,
    pub ffn_up:      Vec<f32>,
    pub norm_w:      Vec<f32>,
    pub bias_tmp:    Vec<f32>,
    pub row_tmp:     Vec<f32>,
    pub name_buf:    [u8; 64],
}

impl ForwardScratch {
    pub fn new(cfg: &ModelConfig, max_seq: usize) -> Self {
        let d  = cfg.d_model;
        let nh = cfg.n_heads;
        let nk = cfg.n_kv_heads;
        let hd = cfg.head_dim;
        let fd = cfg.ffn_dim;
        let max_bias = (nh * hd).max(nk * hd);
        let max_row  = d.max(fd).max(nh * hd);
        Self {
            x:           vec![0f32; d],
            xb:          vec![0f32; d],
            xb2:         vec![0f32; d],
            q:           vec![0f32; nh * hd],
            k_proj:      vec![0f32; nk * hd],
            v_proj:      vec![0f32; nk * hd],
            attn_out:    vec![0f32; nh * hd],
            attn_scores: vec![0f32; max_seq],
            ffn_gate:    vec![0f32; fd],
            ffn_up:      vec![0f32; fd],
            norm_w:      vec![0f32; d],
            bias_tmp:    vec![0f32; max_bias],
            row_tmp:     vec![0f32; max_row],
            name_buf:    [0u8; 64],
        }
    }
}

// ── Name builder ──────────────────────────────────────────────────────────────

fn layer_name<'a>(layer: usize, suffix: &str, buf: &'a mut [u8; 64]) -> &'a str {
    let prefix = b"blk.";
    let mut i  = 0usize;
    for &b in prefix { buf[i] = b; i += 1; }
    if layer >= 10 { buf[i] = b'0' + (layer / 10) as u8; i += 1; }
    buf[i] = b'0' + (layer % 10) as u8; i += 1;
    buf[i] = b'.'; i += 1;
    for &b in suffix.as_bytes() {
        if i >= 63 { break; }
        buf[i] = b; i += 1;
    }
    buf[i] = 0;
    core::str::from_utf8(&buf[..i]).unwrap_or("unknown")
}

// ── Helper: dequant a row from a pre-fetched tensor slice ─────────────────────

#[inline(always)]
fn row_dequant(ggml_type: u32, data: &[u8], row: usize, cols: usize, out: &mut [f32]) {
    let row_bytes = ggml_type_size(ggml_type, cols);
    let start     = row * row_bytes;
    if start + row_bytes <= data.len() {
        dequant_row(&data[start..start + row_bytes], out, cols, ggml_type);
    }
}

// ── Forward pass ──────────────────────────────────────────────────────────────

pub fn forward<'a>(
    gguf:    &'a Gguf<'a>,
    cfg:     &ModelConfig,
    kv:      &mut KVCache,
    token:   usize,
    pos:     usize,
    logits:  &mut Vec<f32>,
    scratch: &mut ForwardScratch,
) {
    let d  = cfg.d_model;
    let nh = cfg.n_heads;
    let nk = cfg.n_kv_heads;
    let hd = cfg.head_dim;
    let fd = cfg.ffn_dim;

    // 1. Embedding lookup — find tensor once
    {
        if let Some((info, data)) = gguf.get_tensor("token_embd.weight") {
            row_dequant(info.ggml_type, data, token, d, &mut scratch.x[..d]);
        }
    }

    // 2. Transformer layers
    for layer in 0..cfg.n_layers {

        // ── 2a. Attention norm ────────────────────────────────────────────────
        {
            let nm = layer_name(layer, "attn_norm.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                dequant_row(data, &mut scratch.norm_w[..d], d, info.ggml_type);
            }
            rms_norm(&mut scratch.xb, &scratch.x[..d], &scratch.norm_w[..d]);
        }

        // ── 2b. Q projection [n_heads*head_dim, d_model] — one lookup ─────────
        {
            let nm = layer_name(layer, "attn_q.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                let t = info.ggml_type;
                for row in 0..nh * hd {
                    row_dequant(t, data, row, d, &mut scratch.row_tmp[..d]);
                    scratch.q[row] = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
                }
            }
            // Q bias
            let nm_b = layer_name(layer, "attn_q.bias", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm_b) {
                dequant_row(data, &mut scratch.bias_tmp[..nh * hd], nh * hd, info.ggml_type);
                for i in 0..nh * hd { scratch.q[i] += scratch.bias_tmp[i]; }
            }
        }

        // ── 2c. K projection [n_kv_heads*head_dim, d_model] ──────────────────
        {
            let nm = layer_name(layer, "attn_k.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                let t = info.ggml_type;
                for row in 0..nk * hd {
                    row_dequant(t, data, row, d, &mut scratch.row_tmp[..d]);
                    scratch.k_proj[row] = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
                }
            }
            let nm_b = layer_name(layer, "attn_k.bias", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm_b) {
                dequant_row(data, &mut scratch.bias_tmp[..nk * hd], nk * hd, info.ggml_type);
                for i in 0..nk * hd { scratch.k_proj[i] += scratch.bias_tmp[i]; }
            }
        }

        // ── 2d. V projection [n_kv_heads*head_dim, d_model] ──────────────────
        {
            let nm = layer_name(layer, "attn_v.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                let t = info.ggml_type;
                for row in 0..nk * hd {
                    row_dequant(t, data, row, d, &mut scratch.row_tmp[..d]);
                    scratch.v_proj[row] = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
                }
            }
            let nm_b = layer_name(layer, "attn_v.bias", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm_b) {
                dequant_row(data, &mut scratch.bias_tmp[..nk * hd], nk * hd, info.ggml_type);
                for i in 0..nk * hd { scratch.v_proj[i] += scratch.bias_tmp[i]; }
            }
        }

        // ── 2e. RoPE ──────────────────────────────────────────────────────────
        for h in 0..nh { rope_apply(&mut scratch.q[h*hd..(h+1)*hd],     pos, hd, cfg.rope_freq); }
        for h in 0..nk { rope_apply(&mut scratch.k_proj[h*hd..(h+1)*hd], pos, hd, cfg.rope_freq); }

        // ── 2f. Write K/V into cache ──────────────────────────────────────────
        for h in 0..nk {
            kv.k_slice_mut(layer, h, pos).copy_from_slice(&scratch.k_proj[h*hd..(h+1)*hd]);
            kv.v_slice_mut(layer, h, pos).copy_from_slice(&scratch.v_proj[h*hd..(h+1)*hd]);
        }

        // ── 2g. Multi-head attention (GQA) ────────────────────────────────────
        let scale = fast_inv_sqrt(hd as f32);
        for h in 0..nh {
            let kv_h = h / (nh / nk);
            let q_h  = &scratch.q[h*hd..(h+1)*hd];
            let attn_s = &mut scratch.attn_scores[..pos + 1];
            for t in 0..=pos {
                attn_s[t] = dot(q_h, kv.k_slice(layer, kv_h, t)) * scale;
            }
            softmax(attn_s);
            let out_h = &mut scratch.attn_out[h*hd..(h+1)*hd];
            out_h.fill(0.0);
            for t in 0..=pos {
                let w   = scratch.attn_scores[t];
                let v_t = kv.v_slice(layer, kv_h, t);
                for i in 0..hd { out_h[i] += w * v_t[i]; }
            }
        }

        // ── 2h. Output projection [d_model, n_heads*head_dim] ────────────────
        {
            let nm = layer_name(layer, "attn_output.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                let t = info.ggml_type;
                for row in 0..d {
                    row_dequant(t, data, row, nh * hd, &mut scratch.row_tmp[..nh * hd]);
                    scratch.xb2[row] = dot(&scratch.attn_out[..nh*hd], &scratch.row_tmp[..nh*hd]);
                }
            }
        }

        // ── 2i. Residual ──────────────────────────────────────────────────────
        for i in 0..d { scratch.x[i] += scratch.xb2[i]; }

        // ── 2j. FFN norm ──────────────────────────────────────────────────────
        {
            let nm = layer_name(layer, "ffn_norm.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                dequant_row(data, &mut scratch.norm_w[..d], d, info.ggml_type);
            }
            rms_norm(&mut scratch.xb, &scratch.x[..d], &scratch.norm_w[..d]);
        }

        // ── 2k. FFN gate + up projections [ffn_dim, d_model] ─────────────────
        {
            let nm_gate = layer_name(layer, "ffn_gate.weight", &mut scratch.name_buf);
            let gate_tensor = gguf.get_tensor(nm_gate);
            let nm_up = layer_name(layer, "ffn_up.weight", &mut scratch.name_buf);
            let up_tensor = gguf.get_tensor(nm_up);

            if let (Some((gi, gd)), Some((ui, ud))) = (gate_tensor, up_tensor) {
                let gt = gi.ggml_type;
                let ut = ui.ggml_type;
                for row in 0..fd {
                    row_dequant(gt, gd, row, d, &mut scratch.row_tmp[..d]);
                    scratch.ffn_gate[row] = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
                    row_dequant(ut, ud, row, d, &mut scratch.row_tmp[..d]);
                    scratch.ffn_up[row]   = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
                }
            }
            // SwiGLU
            for i in 0..fd { scratch.ffn_gate[i] = silu(scratch.ffn_gate[i]) * scratch.ffn_up[i]; }
        }

        // ── 2l. FFN down projection [d_model, ffn_dim] ───────────────────────
        {
            let nm = layer_name(layer, "ffn_down.weight", &mut scratch.name_buf);
            if let Some((info, data)) = gguf.get_tensor(nm) {
                let t = info.ggml_type;
                for row in 0..d {
                    row_dequant(t, data, row, fd, &mut scratch.row_tmp[..fd]);
                    scratch.xb2[row] = dot(&scratch.ffn_gate[..fd], &scratch.row_tmp[..fd]);
                }
            }
        }

        // ── 2m. Residual ──────────────────────────────────────────────────────
        for i in 0..d { scratch.x[i] += scratch.xb2[i]; }
    }

    // 3. Final norm
    {
        if let Some((info, data)) = gguf.get_tensor("output_norm.weight") {
            dequant_row(data, &mut scratch.norm_w[..d], d, info.ggml_type);
        }
        rms_norm(&mut scratch.xb, &scratch.x[..d], &scratch.norm_w[..d]);
    }

    // 4. LM head projection [vocab_size, d_model]
    let v = cfg.vocab_size;
    logits.resize(v, 0.0);
    {
        // Try output.weight first; fall back to tied token_embd.weight
        let (ot, od, nm) = if let Some(r) = gguf.get_tensor("output.weight") {
            (r.0.ggml_type, r.1, "output.weight")
        } else if let Some(r) = gguf.get_tensor("token_embd.weight") {
            (r.0.ggml_type, r.1, "token_embd.weight")
        } else {
            return;
        };
        let _ = nm; // suppress unused
        for row in 0..v {
            row_dequant(ot, od, row, d, &mut scratch.row_tmp[..d]);
            logits[row] = dot(&scratch.xb[..d], &scratch.row_tmp[..d]);
        }
    }
}

// ── Token generation ──────────────────────────────────────────────────────────

pub fn generate<'a, F: FnMut(u32)>(
    gguf:      &'a Gguf<'a>,
    cfg:       &ModelConfig,
    kv:        &mut KVCache,
    scratch:   &mut ForwardScratch,
    prompt:    &[u32],
    max_new:   usize,
    mut on_token: F,
) {
    let mut logits = Vec::with_capacity(cfg.vocab_size);
    let mut pos    = 0usize;

    // Seed with BOS if prompt is empty
    let seed_tok;
    let tokens: &[u32] = if prompt.is_empty() {
        seed_tok = cfg.bos;
        core::slice::from_ref(&seed_tok)
    } else {
        prompt
    };

    // Process prompt — after the last token, logits hold P(next | prompt)
    for &tok in tokens {
        if pos >= kv.max_seq { break; }
        forward(gguf, cfg, kv, tok as usize, pos, &mut logits, scratch);
        pos += 1;
    }

    // ── Sliding-window history for repetition penalty ────────────────────
    // We track the last RP_WINDOW generated tokens.  Before each argmax we
    // multiply their logits by (1/RP_PENALTY) (i.e. divide by ~1.18) so the
    // model is nudged away from looping on the same token / bigram.  This
    // is cheap, deterministic, and dramatically improves coherence at the
    // tail of long greedy responses.
    const RP_WINDOW:  usize = 32;
    const RP_PENALTY: f32   = 1.18;

    let mut history: [u32; RP_WINDOW] = [u32::MAX; RP_WINDOW];
    let mut hist_n: usize = 0;

    // Generate: logits already valid from the final prompt step
    for _ in 0..max_new {
        if logits.is_empty() || pos >= kv.max_seq { break; }

        // Apply repetition penalty.
        let n = hist_n.min(RP_WINDOW);
        for k in 0..n {
            let id = history[k] as usize;
            if id < logits.len() {
                let v = logits[id];
                logits[id] = if v > 0.0 { v / RP_PENALTY } else { v * RP_PENALTY };
            }
        }

        let next = argmax(&logits) as u32;
        if next == cfg.eos { break; }
        on_token(next);

        // Record into the sliding window.
        let slot = hist_n % RP_WINDOW;
        history[slot] = next;
        hist_n += 1;

        forward(gguf, cfg, kv, next as usize, pos, &mut logits, scratch);
        pos += 1;
    }
}

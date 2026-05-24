pub struct ModelWeights<'a> {
    pub token_embd: &'a [f32],
    pub output_norm: &'a [f32],
    pub output_weight: &'a [f32],
    // Aggiungeremo i pesi dei layer qui...
}

pub struct StateVector {
    pub x: [f32; 3584], // Dimensione di Qwen 7B (d_model)
}

impl<'a> ModelWeights<'a> {
    pub fn forward(&self, token_id: usize) {
        // Qui implementeremo il ciclo di inferenza:
        // 1. Embedding lookup
        // 2. Loop sui layer
        // 3. Norm + Logits
    }
}

pub struct Tokenizer<'a> {
    data: &'a [u8],
    count: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn from_gguf_array(array_data: &'a [u8], count: usize) -> Self {
        Self {
            data: array_data,
            count,
        }
    }

    pub fn get_token(&self, id: usize) -> Option<&'a str> {
        if id >= self.count { return None; }
        
        let mut pos = 0;
        for i in 0..self.count {
            if pos + 8 > self.data.len() { return None; }
            let len = unsafe { *(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if i == id {
                return core::str::from_utf8(&self.data[pos..pos+len]).ok();
            }
            pos += len;
        }
        None
    }

    /// Ricerca grezza (molto lenta, ma per ora ci serve per convertire l'input utente)
    pub fn find_id(&self, token: &str) -> Option<usize> {
        let mut pos = 0;
        for i in 0..self.count {
            if pos + 8 > self.data.len() { return None; }
            let len = unsafe { *(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if let Ok(s) = core::str::from_utf8(&self.data[pos..pos+len]) {
                if s == token {
                    return Some(i);
                }
            }
            pos += len;
        }
        None
    }
}

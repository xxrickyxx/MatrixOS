//! Cheap keyword-based intent classifier that picks which model
//! (chat vs code) should answer a given user prompt.
//!
//! No allocation, no regex — just substring matching against a tiny
//! curated vocabulary.  Tie-breaker is the chat model: if no code
//! keywords match strongly enough, default to chat.

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Chat,
    Code,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self { Mode::Chat => "CHAT", Mode::Code => "CODE" }
    }
}

const CODE_KEYWORDS: &[&[u8]] = &[
    b"code",   b"codice", b"funct", b"funz",
    b"class",  b"struct", b"enum",  b"trait",
    b"impl",   b"fn ",    b"def ",  b"pub ",
    b"rust",   b"python", b"java",  b"javascript",
    b"typescript", b"golang", b"cpp", b"c++", b" c ",
    b"html", b"css", b"sql", b"bash", b"shell",
    b"bug",    b"debug",  b"compil", b"linker",
    b"async",  b"await",  b"thread", b"mutex",
    b"return", b"import", b"export", b"const ",
    b"variable", b"variabile", b"pointer", b"puntator",
    b"refactor", b"refactori", b"loop",  b"ciclo",
    b"algorithm", b"algoritm",
    b"matrix", b"vector",  b"array",
    b"linker", b".rs",     b".py", b".js", b".ts",
    b"kernel", b"driver",  b"interrupt",
    b"protocol", b"protocoll",
    b"binary", b"binari",  b"compile", b"compila",
    b"git ",   b"github",  b"commit",
    b"regex",  b"json",    b"xml",  b"yaml",
];

/// Score a prompt and pick a mode.  Returns the chosen mode + the
/// score (0..=N) so the UI can show why the switch happened.
pub fn classify(prompt: &[u8]) -> (Mode, u32) {
    // ASCII lowercasing on the stack (truncated to 256 bytes; long
    // prompts still get a representative scan of their head).
    let mut lower = [0u8; 256];
    let n = prompt.len().min(256);
    for i in 0..n {
        let b = prompt[i];
        lower[i] = if (b'A'..=b'Z').contains(&b) { b + 32 } else { b };
    }
    let buf = &lower[..n];

    let mut score = 0u32;
    for kw in CODE_KEYWORDS {
        if buf.windows(kw.len()).any(|w| w == *kw) {
            score += 1;
        }
    }

    // 2 distinct keyword hits = strong enough to flip into code mode
    if score >= 2 {
        (Mode::Code, score)
    } else {
        (Mode::Chat, score)
    }
}

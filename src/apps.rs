//! MatrixOS app framework.
//!
//! An "app" is a Recipe — a tiny list of commands the kernel knows how to
//! execute.  The chat AI emits recipes (plain text); the code AI then
//! validates the text; the kernel parses + registers only if both pass.
//!
//! The DSL is intentionally minimal so it can't escape kernel space:
//!
//!   # comment
//!   say <text>          print text (supports $tokens, $i, $time)
//!   info                print model & system stats
//!   time                print TSC tick counter
//!   tokens              print total tokens generated this session
//!   nic                 print NIC label
//!   usb                 print USB label
//!   pause <n>           busy-wait n × 100k cycles
//!   clear               clear chat scrollback
//!   loop <n>            begin loop body (n ≤ 64), exposes $i
//!   end                 end of loop body
//!
//! No memory ops, no jumps, no syscalls — only side-effects on the
//! terminal & status bar.  Recipes have a 64-line cap and a 64-iter loop
//! cap, so a malicious / hallucinated recipe can't hang the kernel.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::vga;

pub const MAX_RECIPE_LINES: usize = 64;
pub const MAX_LOOP_ITERS:   u32   = 64;
pub const MAX_APPS:         usize = 16;

#[derive(Clone)]
pub enum Cmd {
    Say(String),
    Info,
    Time,
    Tokens,
    Nic,
    Usb,
    Pause(u32),
    Clear,
    LoopBegin(u32),
    LoopEnd,
}

#[derive(Clone)]
pub struct App {
    pub name:   String,
    pub recipe: Vec<Cmd>,
    pub author: AppAuthor,
    pub text:   String,   // original recipe source, kept so we can save/load
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum AppAuthor {
    Builtin = 0,
    AiChat  = 1,
    AiCode  = 2,
    User    = 3,
}

impl AppAuthor {
    pub fn label(self) -> &'static str {
        match self {
            AppAuthor::Builtin => "builtin",
            AppAuthor::AiChat  => "chat-ai",
            AppAuthor::AiCode  => "code-ai",
            AppAuthor::User    => "user",
        }
    }
}

// ── Parser ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub msg:  &'static str,
}

pub fn parse_recipe(text: &str) -> Result<Vec<Cmd>, ParseError> {
    let mut out: Vec<Cmd> = Vec::new();
    let mut loop_depth = 0usize;

    for (idx, raw) in text.lines().enumerate() {
        if out.len() >= MAX_RECIPE_LINES {
            return Err(ParseError { line: idx + 1, msg: "recipe too long" });
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }

        // Split off the first whitespace-delimited token as command,
        // remainder is argument.
        let (cmd_tok, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None    => (line, ""),
        };

        let cmd = match cmd_tok {
            "say"    => Cmd::Say(String::from(rest)),
            "info"   => Cmd::Info,
            "time"   => Cmd::Time,
            "tokens" => Cmd::Tokens,
            "nic"    => Cmd::Nic,
            "usb"    => Cmd::Usb,
            "clear"  => Cmd::Clear,
            "pause"  => {
                let n = parse_u32(rest).ok_or(ParseError { line: idx + 1, msg: "pause needs an integer" })?;
                Cmd::Pause(n.min(50))
            }
            "loop"   => {
                let n = parse_u32(rest).ok_or(ParseError { line: idx + 1, msg: "loop needs an integer" })?;
                if n > MAX_LOOP_ITERS {
                    return Err(ParseError { line: idx + 1, msg: "loop count too large" });
                }
                loop_depth += 1;
                if loop_depth > 1 {
                    return Err(ParseError { line: idx + 1, msg: "nested loops not supported" });
                }
                Cmd::LoopBegin(n)
            }
            "end"    => {
                if loop_depth == 0 {
                    return Err(ParseError { line: idx + 1, msg: "'end' without 'loop'" });
                }
                loop_depth -= 1;
                Cmd::LoopEnd
            }
            _ => return Err(ParseError { line: idx + 1, msg: "unknown command" }),
        };
        out.push(cmd);
    }

    if loop_depth != 0 {
        return Err(ParseError { line: 0, msg: "missing 'end' for open loop" });
    }
    Ok(out)
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let mut n: u32 = 0;
    for b in s.bytes() {
        if !(b'0'..=b'9').contains(&b) { return None; }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(n)
}

// ── Interpreter ──────────────────────────────────────────────────────────────

/// Snapshot of system state the interpreter can substitute into `say` strings.
pub struct RuntimeCtx<'a> {
    pub total_tokens: u32,
    pub tsc:          u64,
    pub nic:          &'a str,
    pub usb:          &'a str,
    pub model_name:   &'a str,
    pub d_model:      u32,
    pub n_layers:     u32,
}

pub fn run_recipe(cmds: &[Cmd], ctx: &RuntimeCtx) {
    let mut pc = 0usize;
    while pc < cmds.len() {
        match &cmds[pc] {
            Cmd::Say(text) => {
                emit_substituted(text, ctx, 0);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Info => {
                vga::term_puts("[app] model: ", vga::C_DIM);
                vga::term_puts(ctx.model_name, vga::C_CYAN);
                vga::term_puts("  d=", vga::C_DIM);
                vga::term_putu32(ctx.d_model, vga::C_ORANGE);
                vga::term_puts(" L=", vga::C_DIM);
                vga::term_putu32(ctx.n_layers, vga::C_ORANGE);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Time => {
                vga::term_puts("[app] tsc: ", vga::C_DIM);
                // TSC is 64-bit, draw as hex via existing helper
                unsafe {
                    vga::draw_hex(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                                  0, 0, ctx.tsc, vga::C_ORANGE);
                }
                // Also push a low-32-bit decimal for readability
                vga::term_putu32(ctx.tsc as u32, vga::C_ORANGE);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Tokens => {
                vga::term_puts("[app] tokens: ", vga::C_DIM);
                vga::term_putu32(ctx.total_tokens, vga::C_CYAN);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Nic => {
                vga::term_puts("[app] nic: ", vga::C_DIM);
                vga::term_puts(ctx.nic, vga::C_CYAN);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Usb => {
                vga::term_puts("[app] usb: ", vga::C_DIM);
                vga::term_puts(ctx.usb, vga::C_CYAN);
                vga::term_puts("\n", 0);
                pc += 1;
            }
            Cmd::Pause(n) => {
                for _ in 0..*n {
                    for _ in 0..100_000 {
                        unsafe { core::arch::asm!("nop"); }
                    }
                }
                pc += 1;
            }
            Cmd::Clear => {
                vga::clear_chat();
                pc += 1;
            }
            Cmd::LoopBegin(n) => {
                // Find matching End.
                let mut end_pc = pc + 1;
                while end_pc < cmds.len() && !matches!(cmds[end_pc], Cmd::LoopEnd) {
                    end_pc += 1;
                }
                let body = &cmds[pc + 1..end_pc];
                for i in 0..*n {
                    for c in body {
                        match c {
                            Cmd::Say(text) => {
                                emit_substituted(text, ctx, i);
                                vga::term_puts("\n", 0);
                            }
                            Cmd::Info   => { run_recipe(&[Cmd::Info],   ctx); }
                            Cmd::Time   => { run_recipe(&[Cmd::Time],   ctx); }
                            Cmd::Tokens => { run_recipe(&[Cmd::Tokens], ctx); }
                            Cmd::Nic    => { run_recipe(&[Cmd::Nic],    ctx); }
                            Cmd::Usb    => { run_recipe(&[Cmd::Usb],    ctx); }
                            Cmd::Pause(n) => { run_recipe(&[Cmd::Pause(*n)], ctx); }
                            Cmd::Clear  => { vga::clear_chat(); }
                            _ => {}    // nested loop disallowed by parser
                        }
                    }
                }
                pc = end_pc + 1;
            }
            Cmd::LoopEnd => { pc += 1; }   // shouldn't be reached
        }
    }
}

/// Print `text` with simple $var substitution.
fn emit_substituted(text: &str, ctx: &RuntimeCtx, i: u32) {
    let bytes = text.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] == b'$' && j + 1 < bytes.len() {
            // Match a known variable name (longest first)
            let tail = &text[j + 1..];
            if tail.starts_with("tokens") {
                vga::term_putu32(ctx.total_tokens, vga::C_CYAN);
                j += 1 + 6;
                continue;
            } else if tail.starts_with("time") {
                vga::term_putu32(ctx.tsc as u32, vga::C_ORANGE);
                j += 1 + 4;
                continue;
            } else if tail.starts_with("nic") {
                vga::term_puts(ctx.nic, vga::C_CYAN);
                j += 1 + 3;
                continue;
            } else if tail.starts_with("usb") {
                vga::term_puts(ctx.usb, vga::C_CYAN);
                j += 1 + 3;
                continue;
            } else if tail.starts_with("model") {
                vga::term_puts(ctx.model_name, vga::C_CYAN);
                j += 1 + 5;
                continue;
            } else if tail.starts_with('i') {
                vga::term_putu32(i, vga::C_ORANGE);
                j += 1 + 1;
                continue;
            }
        }
        vga::term_putc(bytes[j], vga::C_AI);
        j += 1;
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

pub struct Registry {
    apps: Vec<App>,
}

impl Registry {
    pub const fn new() -> Self { Self { apps: Vec::new() } }

    pub fn install_builtins(&mut self) {
        const HELLO: &str = "say Hello from MatrixOS!\nsay Running on $model.\ninfo";
        const SYSINFO: &str = "say --- system info ---\ninfo\nnic\nusb\ntokens\nsay -------------------";
        const TICK: &str = "say starting ticker:\nloop 5\nsay   tick $i\npause 2\nend\nsay done.";
        for (name, text) in [("hello", HELLO), ("sysinfo", SYSINFO), ("tick", TICK)] {
            self.apps.push(App {
                name:   String::from(name),
                recipe: parse_recipe(text).unwrap_or_default(),
                author: AppAuthor::Builtin,
                text:   String::from(text),
            });
        }
    }

    pub fn find(&self, name: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.name == name)
    }

    pub fn register(&mut self, app: App) -> Result<(), &'static str> {
        if self.apps.len() >= MAX_APPS { return Err("registry full"); }
        if self.find(&app.name).is_some() { return Err("name already exists"); }
        self.apps.push(app);
        Ok(())
    }

    pub fn list(&self) -> &[App] { &self.apps }
}

//! Executor: turns Planner Actions into kernel-API calls.
//!
//! User → LLM → Planner → **Executor** → OS
//!
//! Each `Action` carries a verb + (key, value) args.  Dispatch by verb,
//! look up keys, call the matching kernel API, capture a one-line result.
//! Halts on first hard error (executor returns the partial trail so the
//! caller can show the user what got done).

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::{apps, audio, fs, irq, planner::Action, rng, vga};

pub struct StepResult {
    pub action: String,    // verb that ran
    pub ok:     bool,
    pub detail: String,    // human-readable outcome (one line)
}

/// Drive a plan to completion.  `registry` lets us run apps + read settings.
/// Returns the per-step results in execution order; the caller decides how
/// to display them.
pub fn run_plan(
    actions: &[Action],
    registry: &mut apps::Registry,
    max_chat: &mut usize,
    max_code: &mut usize,
    auto_pick: &mut bool,
) -> Vec<StepResult> {
    let mut out: Vec<StepResult> = Vec::with_capacity(actions.len());

    for (i, act) in actions.iter().enumerate() {
        // Echo what we're about to do.
        vga::term_puts("[plan ", vga::C_DIM);
        vga::term_putu32((i + 1) as u32, vga::C_ORANGE);
        vga::term_puts("/", vga::C_DIM);
        vga::term_putu32(actions.len() as u32, vga::C_ORANGE);
        vga::term_puts("] ", vga::C_DIM);
        vga::term_puts(&act.verb, vga::C_CYAN);
        for (k, v) in &act.args {
            vga::term_puts(" ", 0);
            vga::term_puts(k, vga::C_DIM);
            vga::term_puts("=", vga::C_DIM);
            vga::term_puts(v, vga::C_TEXT);
        }
        vga::term_puts("\n", 0);

        let res = dispatch(act, registry, max_chat, max_code, auto_pick);

        let colour = if res.ok { vga::C_GREEN } else { vga::C_RED };
        vga::term_puts("        -> ", vga::C_DIM);
        vga::term_puts(&res.detail, colour);
        vga::term_puts("\n", 0);

        let halt = !res.ok;
        out.push(res);
        if halt { break; }
    }

    out
}

fn dispatch(
    act: &Action,
    registry: &mut apps::Registry,
    max_chat: &mut usize,
    max_code: &mut usize,
    auto_pick: &mut bool,
) -> StepResult {
    let verb = act.verb.as_str();
    let v = String::from(verb);

    match verb {
        // ── Output ──────────────────────────────────────────────────────
        "say" => {
            let text = act.get("text").unwrap_or("");
            vga::term_puts(text, vga::C_TEXT);
            vga::term_puts("\n", 0);
            ok(v, alloc::format!("printed {} bytes", text.len()))
        }
        "clear" => {
            vga::clear_chat();
            ok(v, String::from("screen cleared"))
        }
        "beep" => {
            let freq = act.get("freq").and_then(|s| s.parse::<u32>().ok()).unwrap_or(880);
            let ms   = act.get("ms").and_then(|s| s.parse::<u32>().ok()).unwrap_or(150);
            audio::beep_blocking(freq, ms);
            ok(v, alloc::format!("beeped {} Hz for {} ms", freq, ms))
        }
        "wait" => {
            let ms = act.get("ms").and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);
            let until = irq::tick_ms() + ms as u64;
            while irq::tick_ms() < until {
                unsafe { core::arch::asm!("pause"); }
            }
            ok(v, alloc::format!("waited {} ms", ms))
        }

        // ── Settings ────────────────────────────────────────────────────
        "set" => {
            let key = act.get("key").unwrap_or("");
            let val = act.get("value").unwrap_or("");
            match key {
                "max_chat" => match val.parse::<u32>() {
                    Ok(n) => { *max_chat = (n as usize).clamp(32, 512); ok(v, alloc::format!("max_chat={}", *max_chat)) }
                    Err(_) => fail(v, String::from("bad number"))
                },
                "max_code" => match val.parse::<u32>() {
                    Ok(n) => { *max_code = (n as usize).clamp(32, 640); ok(v, alloc::format!("max_code={}", *max_code)) }
                    Err(_) => fail(v, String::from("bad number"))
                },
                "auto" => {
                    *auto_pick = matches!(val, "on" | "ON" | "true" | "1");
                    ok(v, alloc::format!("auto={}", if *auto_pick {"on"} else {"off"}))
                }
                _ => fail(v, alloc::format!("unknown setting key {:?}", key))
            }
        }

        // ── Apps ────────────────────────────────────────────────────────
        "run" => {
            let name = act.get("app").or(act.get("name")).unwrap_or("");
            match registry.find(name) {
                Some(app) => {
                    let app = app.clone();
                    let ctx = apps::RuntimeCtx {
                        total_tokens: 0, tsc: crate::math::rdtsc(),
                        nic: "", usb: "", model_name: "", d_model: 0, n_layers: 0,
                    };
                    apps::run_recipe(&app.recipe, &ctx);
                    ok(v, alloc::format!("ran app {}", name))
                }
                None => fail(v, alloc::format!("no app named {:?}", name))
            }
        }

        // ── Filesystem ──────────────────────────────────────────────────
        "fs_save" => {
            let name = act.get("name").unwrap_or("");
            let body = act.get("body").unwrap_or("");
            if name.is_empty() { return fail(v, String::from("name required")); }
            match fs::save(name, body.as_bytes()) {
                Ok(()) => ok(v, alloc::format!("saved {} ({} bytes)", name, body.len())),
                Err(e) => fail(v, alloc::format!("save: {}", e))
            }
        }
        "fs_load" => {
            let name = act.get("name").unwrap_or("");
            match fs::load(name) {
                Ok(data) => {
                    for &b in &data {
                        if (32..127).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                            vga::term_putc(b, vga::C_TEXT);
                        }
                    }
                    vga::term_puts("\n", 0);
                    ok(v, alloc::format!("read {} ({} bytes)", name, data.len()))
                }
                Err(e) => fail(v, alloc::format!("load: {}", e))
            }
        }
        "fs_rm" => {
            let name = act.get("name").unwrap_or("");
            match fs::delete(name) {
                Ok(()) => ok(v, alloc::format!("removed {}", name)),
                Err(e) => fail(v, alloc::format!("rm: {}", e))
            }
        }
        "fs_list" => {
            match fs::list() {
                Ok(files) => {
                    for (n, sz) in &files {
                        vga::term_puts("  ", 0);
                        vga::term_puts(n, vga::C_GREEN);
                        vga::term_puts("  ", vga::C_DIM);
                        let mut bb = [0u8; 12];
                        let s = vga::u32_to_str(*sz, &mut bb);
                        vga::term_puts(s, vga::C_ORANGE);
                        vga::term_puts(" bytes\n", vga::C_DIM);
                    }
                    ok(v, alloc::format!("{} file(s)", files.len()))
                }
                Err(e) => fail(v, alloc::format!("list: {}", e))
            }
        }

        // ── Random ──────────────────────────────────────────────────────
        "rand" => {
            let n = rng::rand_u64();
            ok(v, alloc::format!("0x{:016X}{}", n, if rng::has_hardware() {" (HW)"} else {" (PRNG)"}))
        }

        // ── Unknown ─────────────────────────────────────────────────────
        _ => fail(v, alloc::format!("unknown verb {:?}", verb))
    }
}

fn ok(verb: String, detail: String) -> StepResult {
    StepResult { action: verb, ok: true, detail }
}
fn fail(verb: String, detail: String) -> StepResult {
    StepResult { action: verb, ok: false, detail }
}

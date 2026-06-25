//! Minimal end-to-end demo: load a tiny ARM program into guest memory, run it,
//! and service its `SVC` as a "host call".
//!
//!   cargo run --example run_arm
//!   RUST_LOG=info cargo run --example run_arm   # also see interpreter diagnostics
//!
//! The program computes (10 + 32), then does `svc #1` — our convention for
//! "print r0 and exit".

use touchhle_arm_interpreter::{Cpu, CpuState, Mem};

fn main() {
    // Route the interpreter's `echo!`/`[INTERP-UNIMPL]` diagnostics to stderr
    // when RUST_LOG is set (e.g. RUST_LOG=info).
    env_logger::init();

    let mut mem = Mem::new();

    const CODE: u32 = 0x1000;
    // mov r0, #10      e3a0000a
    // add r0, r0, #32  e2800020   -> r0 = 42
    // svc #1           ef000001
    #[rustfmt::skip]
    let program: &[u8] = &[
        0x0a, 0x00, 0xa0, 0xe3,
        0x20, 0x00, 0x80, 0xe2,
        0x01, 0x00, 0x00, 0xef,
    ];
    mem.write_bytes(CODE, program);

    let mut cpu = Cpu::new(/* null_page_count = */ 1);
    cpu.regs_mut()[15] = CODE; // PC

    // Run until a host call, an error, or 1000 instructions elapse.
    let mut budget = 1_000u64;
    loop {
        match cpu.run_or_step(&mut mem, Some(&mut budget)) {
            CpuState::Svc(num) => {
                println!("guest issued svc #{num}; r0 = {}", cpu.regs()[0]);
                // A real host would service the call and resume here. We just stop.
                break;
            }
            CpuState::Normal => {
                println!("budget exhausted without a host call");
                break;
            }
            CpuState::Error(e) => {
                eprintln!("cpu error: {e:?} at pc={:#x}", cpu.regs()[15]);
                break;
            }
        }
    }

    assert_eq!(cpu.regs()[0], 42);
    println!("ok");
}

//! Integration tests: assemble a few instructions by hand, run them, check state.
//!
//! Encodings are little-endian machine code (as they'd sit in memory). Comments
//! give the assembly + the big-endian word for cross-checking against an ARM
//! reference.

use touchhle_arm_interpreter::{Cpu, CpuState, Mem, CPSR_THUMB};

/// Run up to `budget` instructions; returns at the first `SVC`/error, or
/// [`CpuState::Normal`] if the budget is exhausted first.
fn run(cpu: &mut Cpu, mem: &mut Mem, budget: u64) -> CpuState {
    let mut budget = budget;
    cpu.run_or_step(mem, Some(&mut budget))
}

#[test]
fn arm_compute_and_svc() {
    let mut mem = Mem::new();
    // mov r0, #42      e3a0002a
    // add r0, r0, #1   e2800001   -> r0 = 43
    // svc #7           ef000007
    mem.write_bytes(
        0x1000,
        &[
            0x2a, 0x00, 0xa0, 0xe3, //
            0x01, 0x00, 0x80, 0xe2, //
            0x07, 0x00, 0x00, 0xef, //
        ],
    );
    let mut cpu = Cpu::new(1); // null page = [0, 0x1000)
    cpu.regs_mut()[15] = 0x1000; // PC

    match run(&mut cpu, &mut mem, 100) {
        CpuState::Svc(7) => {}
        other => panic!("expected Svc(7), got {other:?}"),
    }
    assert_eq!(cpu.regs()[0], 43);
}

#[test]
fn arm_memory_roundtrip() {
    let mut mem = Mem::new();
    // mov r0, #0xAB    e3a000ab
    // str r0, [r1]     e5810000
    // ldr r2, [r1]     e5912000
    // svc #0           ef000000
    mem.write_bytes(
        0x1000,
        &[
            0xab, 0x00, 0xa0, 0xe3, //
            0x00, 0x00, 0x81, 0xe5, //
            0x00, 0x20, 0x91, 0xe5, //
            0x00, 0x00, 0x00, 0xef, //
        ],
    );
    let mut cpu = Cpu::new(1);
    cpu.regs_mut()[15] = 0x1000;
    cpu.regs_mut()[1] = 0x2000; // data address

    assert!(matches!(run(&mut cpu, &mut mem, 100), CpuState::Svc(0)));
    assert_eq!(cpu.regs()[2], 0xAB, "value read back from memory");
    assert_eq!(mem.read_bytes_vec(0x2000, 4), [0xAB, 0, 0, 0], "stored bytes");
}

#[test]
fn thumb_compute() {
    let mut mem = Mem::new();
    // (Thumb)
    // movs r0, #5      2005
    // adds r0, #3      3003     -> r0 = 8
    mem.write_bytes(0x1000, &[0x05, 0x20, 0x03, 0x30]);
    let mut cpu = Cpu::new(1);
    cpu.regs_mut()[15] = 0x1000;
    cpu.set_cpsr(cpu.cpsr() | CPSR_THUMB); // enter Thumb mode

    // Step exactly the 2 instructions, then stop on budget.
    let mut budget = 2u64;
    assert!(matches!(
        cpu.run_or_step(&mut mem, Some(&mut budget)),
        CpuState::Normal
    ));
    assert_eq!(budget, 0);
    assert_eq!(cpu.regs()[0], 8);
}

#[test]
fn null_pointer_access_is_an_error() {
    let mut mem = Mem::new();
    // ldr r0, [r1]   with r1 = 0  -> null-segment read -> MemoryError
    // e5910000
    mem.write_bytes(0x1000, &[0x00, 0x00, 0x91, 0xe5]);
    let mut cpu = Cpu::new(1);
    cpu.regs_mut()[15] = 0x1000;
    cpu.regs_mut()[1] = 0; // null

    assert!(matches!(
        cpu.run_or_step(&mut mem, None), // single step
        CpuState::Error(touchhle_arm_interpreter::CpuError::MemoryError)
    ));
}

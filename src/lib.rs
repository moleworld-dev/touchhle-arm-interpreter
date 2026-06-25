/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! # touchhle-arm-interpreter
//!
//! A pure-Rust **ARMv7-A (32-bit)** CPU interpreter — ARM (A32), Thumb / Thumb-2
//! (T16/T32) and VFP — extracted from [touchHLE](https://github.com/touchHLE/touchHLE).
//!
//! It is a classic **decode-and-execute interpreter**: no JIT, no binary
//! translation, no executable memory. That is the whole point — modern iOS
//! (18.4+/A17 "TXM") forbids sideloaded apps from executing JIT pages even with a
//! debugger attached, so a software interpreter is the only way to run guest ARM
//! code there. It works anywhere Rust does.
//!
//! ## Quick start
//!
//! ```no_run
//! use touchhle_arm_interpreter::{Cpu, Mem, CpuState};
//!
//! let mut mem = Mem::new();                 // 4 GiB lazily-committed guest space
//! mem.write_bytes(0x1000, &[                 // `movs r0, #42 ; svc #0` (ARM)
//!     0x2a, 0x00, 0xa0, 0xe3,
//!     0x00, 0x00, 0x00, 0xef,
//! ]);
//!
//! let mut cpu = Cpu::new(/* null_page_count = */ 1);
//! cpu.regs_mut()[15] = 0x1000;              // PC
//!
//! let mut budget = 1_000u64;
//! loop {
//!     match cpu.run_or_step(&mut mem, Some(&mut budget)) {
//!         CpuState::Svc(n) => { println!("guest svc #{n}, r0 = {}", cpu.regs()[0]); break; }
//!         CpuState::Normal => { if budget == 0 { break; } }
//!         CpuState::Error(e) => panic!("cpu error: {e:?}"),
//!     }
//! }
//! ```
//!
//! ## How it talks to the host
//!
//! * **Memory** goes through [`Mem`] (a flat 4 GiB address space) — or any type
//!   implementing [`mem::GuestMem`] if you want a custom backing store.
//! * **Host calls / "syscalls"**: when the guest executes `SVC #imm`, execution
//!   stops and [`Cpu::run_or_step`] returns [`CpuState::Svc`]. The host inspects
//!   registers, does whatever the call means, writes results back, advances, and
//!   resumes. (touchHLE uses this to implement Objective-C / Foundation / OpenGL
//!   ES etc. as native Rust — the guest never runs real iOS frameworks.)
//!
//! See the crate `README.md` for architecture, ISA coverage and provenance.

// `echo!` is the interpreter's diagnostic output (e.g. the `[INTERP-UNIMPL]`
// line printed when it meets an instruction it doesn't implement yet). Routed to
// the `log` crate so a host can capture it; defined before `mod interpreter` so
// the interpreter and its submodules can see it.
macro_rules! echo {
    () => { log::info!("") };
    ($($arg:tt)+) => { log::info!($($arg)+) };
}

pub mod mem;
mod interpreter;

pub use interpreter::{CpuContext, InterpreterCpu};
pub use mem::{ConstPtr, ConstVoidPtr, GuestMem, Mem, MutPtr, MutVoidPtr, Ptr, VAddr};

/// The ARMv7 CPU. Alias for [`InterpreterCpu`] — the name you'll usually use.
pub type Cpu = InterpreterCpu;

/// Why CPU execution stopped (returned by [`Cpu::run_or_step`]).
#[derive(Debug)]
pub enum CpuState {
    /// Ran out of the tick budget (normal), or executed exactly one instruction
    /// (when stepping, i.e. `ticks == None`).
    Normal,
    /// The guest executed `SVC #imm`. The host should service it and resume.
    /// PC has already been advanced past the `SVC`.
    Svc(u32),
    /// Execution hit an error and halted.
    Error(CpuError),
}

/// A reason CPU execution was interrupted with an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuError {
    /// Bad memory access (e.g. a null-page dereference, or out of bounds).
    MemoryError,
    /// An instruction encoding the interpreter does not implement (logged as
    /// `[INTERP-UNIMPL]` via [`log`]), or an explicitly undefined instruction.
    UndefinedInstruction,
    /// A `BKPT` instruction was executed.
    Breakpoint,
}

/// Register index of the stack pointer (R13).
pub const SP: usize = 13;
/// Register index of the link register (R14).
pub const LR: usize = 14;
/// Register index of the program counter (R15).
pub const PC: usize = 15;
/// CPSR bit: when set, the CPU is in Thumb mode.
pub const CPSR_THUMB: u32 = 0x0000_0020;
/// CPSR bit: when set, the CPU is in user mode.
pub const CPSR_USER_MODE: u32 = 0x0000_0010;

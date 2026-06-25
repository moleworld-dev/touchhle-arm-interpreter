# touchhle-arm-interpreter

一个**纯 Rust 写的 ARMv7-A(32 位)CPU 解释器**——支持 ARM(A32)、Thumb / Thumb-2
(T16 / T32)和 VFP 浮点。它从 [touchHLE](https://github.com/touchHLE/touchHLE)
(一个把老 iPhone OS app 跑在现代设备上的高层模拟器)里抽取、整理而来。

> 一句话:**逐指令 decode-and-execute,没有 JIT、没有二进制翻译、不需要可执行内存**——
> 因此它能跑在那些**禁止 JIT** 的平台上(现代 iOS 旁加载 app),也能跑在任何 Rust 能跑的地方。

[![tests](https://img.shields.io/badge/tests-passing-brightgreen)](#构建与测试)
license: MPL-2.0

---

## 为什么是解释器,而不是 JIT?

绝大多数高性能 CPU 模拟器(QEMU、dynarmic 等)用 **JIT**:把 guest 机器码动态翻译成宿主机器码,
写进一块**可执行内存**再跳进去执行。这快,但前提是操作系统允许"把数据当代码执行"。

**现代 iOS 不允许。** 从 iOS 18.4 / A17(TXM,Trusted Execution Monitor)起,旁加载(sideload)
的 app **即使挂着调试器、设了 `CS_DEBUGGED`,执行 JIT 页仍然 fault**。也就是说,JIT 路线在现代
iPhone 上彻底死了。要在这些设备上跑 guest ARM 代码,**软件解释器是唯一出路**——这正是本 crate
存在的原因。代价是慢(解释器没有 JIT 快),换来的是"哪儿都能跑、无需特权"。

桌面 / 安卓上 touchHLE 用的是另一个后端(C++ 的 [dynarmic](https://github.com/merryhime/dynarmic)
JIT);本 crate 只取**解释器**那一半。

## 它是什么 / 不是什么

**是**:
- 一个 ARMv7-A **用户态**整数 + VFP 指令解释器(ARM / Thumb / Thumb-2)。
- 自带一个扁平 4 GiB guest 地址空间([`Mem`])与一套类型化 guest 指针([`Ptr`])。
- 通过 `SVC` 指令把控制权交还宿主(host-call / "syscall" 机制),宿主可用纯 Rust 实现任何"系统调用"。

**不是**:
- 不是一个完整的 iOS / Mach-O 加载器、动态链接器或 Foundation/UIKit 实现——那些是 touchHLE 上层的事。
- 不是一个**穷尽所有指令**的解释器:它实现了跑通真实游戏所需的指令集,遇到没实现的编码会打印
  `[INTERP-UNIMPL]` 日志并以 [`CpuError::UndefinedInstruction`] 停机(见 [扩展指令](#扩展实现新指令))。
- 不实现特权态 / MMU / 异常向量 / 协处理器(除 VFP/部分系统寄存器外)。

## 快速开始

```rust
use touchhle_arm_interpreter::{Cpu, Mem, CpuState};

let mut mem = Mem::new();                  // 4 GiB 惰性提交的 guest 地址空间
// mov r0, #42 ; svc #0   (ARM,小端机器码)
mem.write_bytes(0x1000, &[
    0x2a, 0x00, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef,
]);

let mut cpu = Cpu::new(/* null_page_count = */ 1); // 低 0x1000 字节为 null 段,捕获空指针
cpu.regs_mut()[15] = 0x1000;               // PC

let mut budget = 1_000u64;                 // 最多跑 1000 条指令
match cpu.run_or_step(&mut mem, Some(&mut budget)) {
    CpuState::Svc(n) => println!("guest svc #{n}, r0 = {}", cpu.regs()[0]), // 打印 42
    CpuState::Normal => println!("预算耗尽"),
    CpuState::Error(e) => panic!("cpu error: {e:?}"),
}
```

可运行示例:`cargo run --example run_arm`(加 `RUST_LOG=info` 还能看到解释器诊断日志)。

## 工作原理

### 取指 → 译码 → 执行

[`Cpu::run_or_step`] 是入口:
- `ticks = Some(budget)`:连续执行,每条指令把 `budget` 减 1,直到预算耗尽 / 遇到 `SVC` / 出错。
- `ticks = None`:**单步**(执行恰好一条指令)。

每条指令:
1. 按当前 CPSR 的 Thumb 位决定取 ARM(4 字节)还是 Thumb(2/4 字节,按 Thumb-2 长度规则判定);
2. 分派到对应译码器(`arm.rs` / `thumb16.rs` / `thumb32.rs` / `vfp.rs`)执行,更新寄存器 / CPSR / 内存;
3. 推进 PC。没有指令缓存、没有基本块——纯逐条解释。

### 宿主调用(SVC)

guest 执行 `SVC #imm` 时,解释器**不**自己解释任何系统调用语义,而是停机并让 [`Cpu::run_or_step`]
返回 [`CpuState::Svc(imm)`](PC 已推过该指令)。宿主据此查寄存器、做这次调用该做的事(读写参数、
调用真实 Rust 实现)、把返回值写回寄存器,然后继续 `run_or_step`。

touchHLE 正是用这一个机制把整个 Objective-C 运行时、Foundation、OpenGL ES 等**全部用 Rust 实现**——
guest 二进制里所有对系统库的调用都被链接器改写成 `SVC`,从不真正执行 iOS 框架代码。你也可以用它实现
你自己的 ABI / syscall 约定。

### 内存模型

[`Mem`] 用**一次 `mmap` 预留整个 32 位(4 GiB)地址空间**,由操作系统**惰性提交**——实际占用的物理内存
只有 guest 真正触碰过的页(老 iPhone app 通常几十~两百多 MB)。guest 地址 `a` 直接映射到宿主 `base + a`,
没有页表。低 `null_page_count × 4 KiB` 字节是 **null 段**,任何访问都判为空指针解引用而失败。

想换后端(更小的 `Vec`、自定义 MMU、快照回滚)?实现 [`GuestMem`] trait 即可——解释器对内存的全部需求
就是这个 trait 上的几个字节读写方法。[`Mem`] 只是开箱即用的默认实现。

### 寄存器与上下文

- 整数寄存器 `R0–R15`:[`Cpu::regs`] / [`Cpu::regs_mut`](`R13`=SP,`R14`=LR,`R15`=PC,见常量 [`SP`]/[`LR`]/[`PC`])。
- CPSR:[`Cpu::cpsr`] / [`Cpu::set_cpsr`](Thumb 位见 [`CPSR_THUMB`])。
- VFP 寄存器组 + FPSCR:[`Cpu::extregs`] / [`Cpu::fpscr`] 等。
- 线程切换:[`CpuContext`] + [`Cpu::swap_context`] 一次性换出 / 换入整套寄存器(宿主自己调度 guest 线程时用)。

## 公共 API 速查

| 调用 | 作用 |
|---|---|
| `Cpu::new(null_page_count)` | 建一个 CPU(返回 `Box<Cpu>`)。`null_page_count` × 4 KiB = null 段大小 |
| `cpu.run_or_step(&mut mem, Some(&mut budget))` | 连续执行至预算耗尽 / SVC / 出错 |
| `cpu.run_or_step(&mut mem, None)` | 单步一条指令 |
| `cpu.regs()` / `cpu.regs_mut()` | `&[u32; 16]` 整数寄存器 |
| `cpu.cpsr()` / `cpu.set_cpsr(v)` | 程序状态寄存器 |
| `cpu.extregs()` / `cpu.fpscr()` … | VFP 状态 |
| `cpu.swap_context(&mut ctx)` | 与 [`CpuContext`] 交换整套状态(线程切换) |
| `cpu.invalidate_cache_range(base, size)` | 解释器下为 no-op(无指令缓存);保留以兼容自改代码 / dyld 重写桩 |
| `Mem::new()` / `mem.write_bytes(addr, &[..])` / `mem.read_bytes_vec(addr, n)` | 建内存、装载代码/数据、回读 |
| `mem.set_null_segment_size(n)` | 设置 null 段(通常加载二进制时设一次) |

## Cargo features

| feature | 默认 | 说明 |
|---|---|---|
| `interp_debug` | 关 | 逐指令调试插桩:最近指令 trace 环 + DERAIL 检测。很慢(~40×),抓"跑飞到非法地址"用 |
| `interp_hb` | 关 | 只开轻量心跳(周期性打印 pc/lr/sp)。~1.05×,抓"卡死"时看 PC 聚成循环体 |
| `moleworld_compat` | 关 | **摩尔庄园(MoleWorld)5.5.0 专用**硬编码加速 / 死循环守卫(按 PC 地址硬编码)。通用用途**别开**;见下 |

> 通用 ARMv7 解释器不应特判某个 PC 地址,所以游戏专用 hack 默认全部关闭——开 `moleworld_compat`
> 才会启用。日志走 [`log`](https://docs.rs/log) facade:宿主选一个 logger(如 `env_logger`)才能看到输出。

## 性能说明

这是解释器,不是 JIT——别拿它跟 dynarmic / QEMU 比吞吐。它的定位是"在禁 JIT 的平台上能跑"。提速思路
(尚未做)包括:PC→译码结果缓存、基本块串联、ITSTATE 缓存等(源码里有 `// P1:` TODO 标记)。
**正常使用务必用 release 构建**,且**别开** `interp_debug` / `interp_hb`(它们给每条指令加固定开销)。

## 扩展:实现新指令

解释器是"够用即实现"的:碰到没实现的编码会打印
`[INTERP-UNIMPL] ...`(经 [`log`])并以 [`CpuError::UndefinedInstruction`] 停机。这条日志就是工作队列:

1. 用 `RUST_LOG=info` 跑你的 guest,收集 `[INTERP-UNIMPL]` 里的编码;
2. 对照 *ARM Architecture Reference Manual*(ARMv7-A/R)解码该指令;
3. 在 `src/interpreter/{arm,thumb16,thumb32,vfp}.rs` 对应译码器里补上分支;
4. 加一条 `tests/` 用例(手写编码 → 跑 → 校验寄存器/内存,本仓库已有范例)。

## 与 touchHLE 的关系(provenance)

本 crate 的解释器源码(`src/interpreter/`)与内存/指针类型(`src/mem.rs`)**整理自 touchHLE**
(<https://github.com/touchHLE/touchHLE>,MPL-2.0)。抽取时所做的改动:

- 把对 `touchHLE` 其余部分的耦合收敛为:[`Mem`] / [`Ptr`]、[`CpuError`] / [`CpuState`]、一个 `echo!` 日志宏;
- `echo!` 改接 [`log`] crate(原本写 touchHLE 的日志文件 / SDL_Log);
- 删掉 `diff.rs` 差分对拍器(它在每条指令上同时跑 dynarmic JIT 做交叉验证,依赖 C++ 工具链,
  对独立解释器无意义);相关 `#[cfg(...)]` 代码块因 feature 不存在而自动编译掉;
- 把若干**摩尔庄园专用**的硬编码 PC 加速 / 守卫挪到 `moleworld_compat` feature 之后(默认关)。

ISA 实现逻辑本身**逐字保真**,未改动语义。

## 构建与测试

```bash
cargo build --release      # 库
cargo test                 # 单测 + 集成测试 + doctest
cargo run --example run_arm

# 调试构建(抓卡死/跑飞):
cargo build --release --features interp_hb
```

依赖:[`log`](https://docs.rs/log)(日志 facade)、[`libc`](https://docs.rs/libc)(4 GiB `mmap`)。
当前 `Mem` 的 mmap 实现走 Unix(macOS / Linux);Windows 需把 `src/mem.rs` 里的 `mmap`/`munmap`
换成 `VirtualAlloc`/`VirtualFree`(或自己实现 [`GuestMem`])。

## 许可证

[MPL-2.0](LICENSE),与上游 touchHLE 一致。

# ADR 0012: Process-Isolated Worker Pool and WASM Sandboxing Re-Evaluation

## Status

Accepted (Phase 4, Milestone M0 / M3)

## Context

Untrusted documents in forensic and eDiscovery pipelines may be malformed or maliciously crafted to trigger out-of-bounds allocations, pathological layout loops, or stack overflows. While Prism is written in safe Rust with bounded decompression limits, an unhandled panic, stack overflow, or allocator abort inside an in-process parser would terminate the host process.

In Phase 0, `crates/prism-sandbox` was a stub placeholder referencing WebAssembly (`wasmtime`). We need real isolation on Windows and Linux today, and an explicit architectural decision on OS process isolation vs. WASM.

## Decision

1. **OS Process Isolation (`prism-sandbox` + `prism-worker`)**:
   - Replace the stub with a warm worker-process pool (`SandboxManager`) spawning `prism-worker` child processes.
   - Parent and worker communicate exclusively over `stdin`/`stdout` using a deterministic, length-prefixed binary framing protocol (`PRWK` 4-byte magic, protocol version, JSON header and payload lengths, then the JSON header and binary payload; see `docs/architecture/sandbox.md`) with no sockets and no shared temporary files.
   - **Per-Job Resource Enforcement**:
     - **Windows**: Assign each worker to a Windows Job Object configured with `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, `JOB_OBJECT_LIMIT_JOB_MEMORY`, `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` (`1`), `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and UI restrictions (`JOB_OBJECT_UILIMIT_HANDLES`, `CLIPBOARD`, `DESKTOP`, `DISPLAYSETTINGS`, `EXITWINDOWS`, `GLOBALATOMS`, `READCLIPBOARD`, `SYSTEMPARAMETERS`, `WRITECLIPBOARD`).
     - **Linux**: Apply `prctl(PR_SET_NO_NEW_PRIVS, 1)`, `setrlimit(RLIMIT_CORE, 0)`, and `setrlimit(RLIMIT_AS)` as a backstop well above the heap limit (the tracking allocator enforces the heap limit itself). CPU time is bounded by the parent's wall-clock timeout, not `RLIMIT_CPU`.
     - **Parent Watchdog**: Enforce wall-clock timeout and maximum output frame size in the parent process, killing and recycling the worker on timeout or crash and returning a structured `SandboxError` (`Timeout`, `MemoryLimit`, `Crashed`, `ProtocolError`, `OutputLimit`).
   - Provide an `InProcessExecutor` implementing the same `JobExecutor` trait for embedded environments that disallow subprocess creation.

2. **WASM Sandboxing Re-Evaluation**:
   - Compiling the full layout and font stack (`fontique`, `harfrust`, `parley`, `tiny-skia`, `krilla`, `hayro`) to `wasm32-unknown-unknown` or `wasm32-wasip1` adds significant binary bloat, single-threaded 4 GiB address space constraints, and 1.5x–2.5x JIT/AOT overhead compared to native OS processes.
   - OS process isolation with Job Objects (Windows) and `rlimit`/`no_new_privs` (Linux) contains memory, time and crashes without native C/C++ runtime dependencies, and a warm worker adds little per-job overhead (the pool tests budget 100–200 ms per warm job; it has not been benchmarked further). WASM sandboxing remains deferred unless a browser-only worker target is needed in a future phase.

## Consequences

- A hostile document that exhausts memory, loops infinitely, overflows the stack, or aborts only terminates its `prism-worker` subprocess; the pool replaces the worker and continues serving subsequent documents.

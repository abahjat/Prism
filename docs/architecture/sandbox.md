# Process Isolation and Worker Pool (`prism-sandbox`)

This document describes `prism-sandbox` and the `prism-worker` subprocess binary.

## 1. Threat Model and Goals

Document parsers handle hostile input (nested archives, decompression bombs, malformed PDFs,
corrupt OLE/CFB streams). Even in safe Rust, such input can cause:

- unbounded heap allocation,
- pathological CPU loops,
- deep recursion that overflows the stack,
- an abort inside a dependency.

`prism-sandbox` runs jobs in a pool of `prism-worker` subprocesses so that such a document kills
only its worker, never the host (`prism-cli`, `prism-server`). The C ABI (`prism-bindings`) and
`InProcessExecutor` run in the host process and rely on panic catching and the parsers' own
limits instead.

## 2. Wire Protocol (`PRWK` v1)

Parent and worker talk only over the worker's stdin and stdout: no sockets, no shared temporary
files. Each frame is:

| Bytes | Content |
| :--- | :--- |
| 4 | magic `b"PRWK"` |
| 2 | protocol version, `u16` little-endian (`1`) |
| 4 | JSON header length, `u32` little-endian (at most 16 MiB) |
| 8 | binary payload length, `u64` little-endian (checked against the output limit before allocating) |
| … | UTF-8 JSON header (`WorkerRequestHeader` / `WorkerResponseHeader`) |
| … | payload (document bytes on request; the rendered artifact on response) |

## 3. Resource Limits

### Heap (all platforms)

`prism-worker` installs a tracking global allocator. When live heap bytes exceed the job's
`max_memory`, or an allocation fails, the worker exits with `EXIT_CODE_MEMORY_LIMIT` (`99`) and
the parent reports `SandboxError::MemoryLimit`. The input document counts toward the limit.

### Windows

Each worker is assigned to a Job Object after it starts (a short window exists before the
assignment) with:

- `JOB_OBJECT_LIMIT_PROCESS_MEMORY` and `JOB_OBJECT_LIMIT_JOB_MEMORY` set to `max_memory`,
- `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1` (no child processes),
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (no orphaned workers),
- `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`, plus `SetErrorMode` in the worker so crashes do
  not raise error-reporting dialogs,
- all `JOB_OBJECT_UILIMIT_*` restrictions.

### Linux / Unix

- `prctl(PR_SET_NO_NEW_PRIVS, 1)` (Linux).
- `setrlimit(RLIMIT_CORE, 0)`.
- `setrlimit(RLIMIT_AS)` as a backstop at `max(4 × max_memory, 1 GiB)`. Address space is not
  heap: thread stacks and allocator arenas reserve far more than they use, so the exact limit is
  left to the tracking allocator.

### Network

There is no OS-level network block. Prism contains no network client, and `cargo deny` keeps the
dependency set reviewed; the worker sets a no-network flag, and the
`HostileBehavior::AttemptNetworkConnect` test only checks that flag. Deployments that need a
hard guarantee should run workers in an egress-blocked network namespace or firewall policy (see
`docs/deployment.md`).

## 4. Worker Pool (`SandboxManager`)

- Keeps up to `pool_size` idle workers.
- Retires a worker after `max_jobs_per_worker` jobs, or when its heap after a job is above half of
  `max_memory`.
- Enforces the wall-clock limit (`max_execution_time`) from the parent: on timeout the worker is
  killed and the job fails with `SandboxError::Timeout`.
- Finds the worker through `SandboxConfig::worker_binary_path`, `PRISM_WORKER_BIN`, or the
  directory of the running executable (and its parent). Debug builds also fall back to the
  workspace's `target/debug`, building the worker there if needed; release builds never run
  `cargo`.
- `InProcessExecutor` implements the same `SandboxExecutor` trait for hosts that cannot spawn
  processes, without isolation.

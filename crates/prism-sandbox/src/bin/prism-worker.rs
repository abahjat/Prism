// SPDX-License-Identifier: AGPL-3.0-only
//! Isolated worker executable (`prism-worker`) managed by `prism_sandbox::SandboxManager`.
//!
//! Reads length-prefixed binary frames (`PRWK` v1) over `stdin`, applies OS and heap resource
//! limits, executes the requested document parsing/rendering/redaction job without network egress,
//! and writes length-prefixed responses to `stdout`.

use prism_sandbox::{
    SandboxError, WorkerRequestHeader, WorkerResponseHeader, WorkerSessionState,
    apply_worker_self_sandbox, current_worker_heap_bytes, execute_wire_request_with_session,
    read_frame, record_worker_alloc, set_worker_heap_limit, write_frame,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, BufReader, BufWriter};
use std::panic::{AssertUnwindSafe, catch_unwind};

struct TrackingAllocator;

// SAFETY: Delegates to `std::alloc::System` while updating atomic heap usage counters.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[allow(clippy::cast_possible_wrap)]
        let size = layout.size() as isize;
        record_worker_alloc(size);
        // SAFETY: Forwarded to `System::alloc`.
        let ptr = unsafe { System.alloc(layout) };
        if ptr.is_null() {
            std::process::exit(prism_sandbox::EXIT_CODE_MEMORY_LIMIT);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        #[allow(clippy::cast_possible_wrap)]
        let size = layout.size() as isize;
        // SAFETY: Forwarded to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) };
        record_worker_alloc(-size);
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn main() {
    let initial_max_mem = std::env::var("PRISM_WORKER_MAX_MEM")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    apply_worker_self_sandbox(initial_max_mem);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let mut session_state: Option<WorkerSessionState> = None;

    loop {
        let frame_res = read_frame::<WorkerRequestHeader, _>(&mut reader, 512 * 1024 * 1024);
        let (req_hdr, payload) = match frame_res {
            Ok(pair) => pair,
            Err(_) => break,
        };

        set_worker_heap_limit(req_hdr.max_memory);

        let exec_res = catch_unwind(AssertUnwindSafe(|| {
            execute_wire_request_with_session(&req_hdr, &payload, &mut session_state)
        }));

        let (resp_hdr, resp_bytes) = match exec_res {
            Ok(Ok(out)) => (
                WorkerResponseHeader {
                    ok: true,
                    content_type: out.content_type,
                    redaction_log_json: out.redaction_log_json,
                    error: None,
                    memory_limit_exceeded: false,
                    heap_bytes: current_worker_heap_bytes(),
                },
                out.data,
            ),
            Ok(Err(SandboxError::MemoryLimit { .. })) => (
                WorkerResponseHeader {
                    ok: false,
                    content_type: "text/plain".to_string(),
                    redaction_log_json: None,
                    error: Some("Memory limit exceeded".to_string()),
                    memory_limit_exceeded: true,
                    heap_bytes: current_worker_heap_bytes(),
                },
                Vec::new(),
            ),
            Ok(Err(err)) => (
                WorkerResponseHeader {
                    ok: false,
                    content_type: "text/plain".to_string(),
                    redaction_log_json: None,
                    error: Some(err.to_string()),
                    memory_limit_exceeded: false,
                    heap_bytes: current_worker_heap_bytes(),
                },
                Vec::new(),
            ),
            Err(panic_payload) => {
                let msg = panic_payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic_payload.downcast_ref::<&str>().copied())
                    .unwrap_or("Worker panic");
                (
                    WorkerResponseHeader {
                        ok: false,
                        content_type: "text/plain".to_string(),
                        redaction_log_json: None,
                        error: Some(format!("Worker panic: {msg}")),
                        memory_limit_exceeded: false,
                        heap_bytes: current_worker_heap_bytes(),
                    },
                    Vec::new(),
                )
            }
        };

        if write_frame(&mut writer, &resp_hdr, &resp_bytes).is_err() {
            break;
        }
    }
}

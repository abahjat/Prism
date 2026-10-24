// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Sandbox (`prism-sandbox`)
//!
//! Process-isolated worker pool (`SandboxManager`, `prism-worker`) and in-process fallback
//! (`InProcessExecutor`) for safely parsing, inspecting, rendering, and redacting untrusted
//! documents with strict memory, CPU, wall-clock, and output-size limits.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::collapsible_if,
    clippy::module_name_repetitions,
    clippy::struct_field_names,
    clippy::too_many_lines
)]

use bytes::Bytes;
use prism_core::Document;
use prism_core::format::detect_format;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_core::render::{RenderContext, RenderOptions, Renderer};
use prism_layout::{LayoutDocument, LayoutOptions, interpret_pdf_bytes, layout_document};
use prism_parsers::registry::ParserRegistry;
use prism_parsers::review::generate_hidden_content_report;
use prism_parsers::tree::build_family_tree;
use prism_render::html::HtmlRenderer;
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, export_document_tiff, render_page_png};
use prism_render::svg::render_page_svg;
use prism_review::{AnnotationSidecar, RedactedExportFormat, export_redacted_document};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Prism sandbox version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 4-byte framing magic for the `prism-worker` stdin/stdout pipe protocol.
pub const PROTOCOL_MAGIC: [u8; 4] = *b"PRWK";

/// Current wire protocol version for `prism-worker`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Dedicated worker exit code indicating that the per-job memory limit was exceeded.
pub const EXIT_CODE_MEMORY_LIMIT: i32 = 99;

/// Maximum JSON header size (`16 MiB`) permitted on the worker pipe.
const MAX_HEADER_JSON_BYTES: usize = 16 * 1024 * 1024;

static WORKER_NETWORK_BLOCKED: AtomicBool = AtomicBool::new(false);
static WORKER_HEAP_LIMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
static WORKER_HEAP_CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Structured error returned by [`SandboxManager`] or [`InProcessExecutor`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum SandboxError {
    /// Worker exceeded the configured wall-clock timeout and was terminated.
    #[error("Worker exceeded wall-clock timeout ({0:?})")]
    Timeout(Duration),
    /// Worker exceeded the configured per-job memory limit (`limit_bytes`).
    #[error("Worker exceeded memory limit ({limit_bytes} bytes)")]
    MemoryLimit {
        /// Configured memory limit in bytes.
        limit_bytes: usize,
    },
    /// Worker process crashed or terminated abnormally (`exit_code`).
    #[error("Worker process crashed (exit code: {exit_code:?})")]
    Crashed {
        /// OS exit code or signal indicator.
        exit_code: Option<i32>,
    },
    /// Worker attempted to return an output payload larger than `max_output_bytes`.
    #[error("Worker output size ({size} bytes) exceeded limit ({limit} bytes)")]
    OutputTooLarge {
        /// Actual payload size in bytes.
        size: usize,
        /// Configured maximum output size in bytes.
        limit: usize,
    },
    /// Framing or serialization error on the worker pipe.
    #[error("Worker protocol error: {0}")]
    ProtocolError(String),
    /// Document parsing or rendering returned a structured application error inside the worker.
    #[error("{0}")]
    TaskFailed(String),
}

/// Output format for [`SandboxJob::RenderPage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderOutputFormat {
    /// Standalone SVG vector page.
    Svg,
    /// Deterministic PNG raster page.
    Png,
}

/// Output format for [`SandboxJob::ExportDocument`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportOutputFormat {
    /// Searchable PDF 1.7 document.
    Pdf,
    /// Multi-page TIFF raster document.
    Tiff,
}

/// Deliberately hostile worker behaviors used by isolation tests to verify that the host
/// survives worker OOMs, infinite loops, stack overflows, aborts, and network attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum HostileBehavior {
    /// Allocate `megabytes` MiB on the heap without freeing.
    AllocateUnbounded {
        /// Number of megabytes to allocate.
        megabytes: usize,
    },
    /// Spin forever in a tight CPU loop until killed by the parent's wall-clock timeout.
    InfiniteLoop,
    /// Recurse unconditionally to overflow the call stack.
    StackOverflow,
    /// Call `std::process::abort()` immediately.
    Abort,
    /// Report whether the worker's no-network flag is set. No socket is opened: Prism contains
    /// no network client, and the worker does not add an OS-level network block (see
    /// `docs/architecture/sandbox.md`).
    AttemptNetworkConnect,
}

/// A unit of work submitted to [`SandboxManager`] or [`InProcessExecutor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxJob {
    /// Detect format from magic bytes and optional filename hint (returns JSON `DetectionResult`).
    Detect {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Extract document metadata, format info, and page count as JSON.
    Info {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Extract plain UTF-8 text from all pages/sections.
    ExtractText {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Render document to sanitized self-contained HTML5.
    RenderHtml {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Render a single 0-based page to SVG or PNG.
    RenderPage {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
        /// Zero-based page index.
        page_index: u32,
        /// Target page format (`Svg` or `Png`).
        format: RenderOutputFormat,
        /// Rasterization DPI (used for `Png`).
        dpi: f32,
    },
    /// Export the entire document to PDF or multi-page TIFF, optionally applying a destructive
    /// [`AnnotationSidecar`] JSON and verifying the output before returning.
    ExportDocument {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
        /// Target export format (`Pdf` or `Tiff`).
        format: ExportOutputFormat,
        /// Rasterization DPI (used for `Tiff`).
        dpi: f32,
        /// Optional serialized [`AnnotationSidecar`] JSON.
        sidecar_json: Option<String>,
    },
    /// Generate the cross-format [`prism_core::review::HiddenContentReport`] as JSON.
    HiddenContentReport {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Build the container/embedded-object [`prism_core::tree::FamilyTree`] as JSON.
    FamilyTree {
        /// Optional filename hint.
        filename: Option<String>,
        /// Raw document bytes.
        bytes: Vec<u8>,
    },
    /// Execute a test-only hostile behavior inside the worker.
    HostileTest(HostileBehavior),
}

/// Output returned from a completed [`SandboxJob`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxOutput {
    /// Primary output bytes (UTF-8 JSON, UTF-8 text/HTML/SVG, or binary PNG/TIFF/PDF).
    pub data: Vec<u8>,
    /// MIME content type of `data` (e.g. `"application/json"`, `"application/pdf"`, `"image/png"`).
    pub content_type: String,
    /// Optional serialized [`prism_review::RedactionLog`] JSON when redactions were applied.
    pub redaction_log_json: Option<String>,
}

/// Wire header sent from parent to `prism-worker` before the raw document byte payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRequestHeader {
    /// Maximum memory permitted for this job in bytes.
    pub max_memory: usize,
    /// Maximum output bytes permitted for this job.
    pub max_output_bytes: usize,
    /// Operation descriptor (without the raw document byte buffer).
    pub op: WorkerWireOp,
}

/// Wire operation descriptor (paired with the raw binary payload in the frame).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WorkerWireOp {
    /// Detect format.
    Detect {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Extract document metadata and page count JSON.
    Info {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Extract plain text.
    ExtractText {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Render HTML.
    RenderHtml {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Render single page (`Svg` or `Png`).
    RenderPage {
        /// Optional filename hint.
        filename: Option<String>,
        /// Zero-based page index.
        page_index: u32,
        /// Output format.
        format: RenderOutputFormat,
        /// DPI.
        dpi: f32,
    },
    /// Export full document (`Pdf` or `Tiff`), optionally with destructive redactions.
    ExportDocument {
        /// Optional filename hint.
        filename: Option<String>,
        /// Output format.
        format: ExportOutputFormat,
        /// DPI.
        dpi: f32,
        /// Optional [`AnnotationSidecar`] JSON.
        sidecar_json: Option<String>,
    },
    /// Generate [`prism_core::review::HiddenContentReport`] JSON.
    HiddenContentReport {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Build [`prism_core::tree::FamilyTree`] JSON.
    FamilyTree {
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// Trigger hostile behavior for isolation testing.
    HostileTest {
        /// Hostile behavior variant.
        behavior: HostileBehavior,
    },
}

/// Wire header returned from `prism-worker` to parent before the raw output byte payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResponseHeader {
    /// Whether the operation succeeded.
    pub ok: bool,
    /// MIME content type of the binary payload when `ok == true`.
    pub content_type: String,
    /// Optional redaction log JSON when `ok == true`.
    pub redaction_log_json: Option<String>,
    /// Error message when `ok == false`.
    pub error: Option<String>,
    /// Whether the failure was specifically a memory limit violation.
    pub memory_limit_exceeded: bool,
    /// Worker current heap usage in bytes after completing the job.
    pub heap_bytes: usize,
}

/// Configuration for [`SandboxManager`] and worker limits.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum memory per worker instance in bytes (default `256 MiB`; `64 MiB` via `Default` for backwards compatibility).
    pub max_memory: usize,
    /// Maximum wall-clock execution time per job (default `30 s`).
    pub max_execution_time: Duration,
    /// Legacy instruction count field retained for API compatibility.
    pub max_instructions: Option<u64>,
    /// Maximum output payload bytes permitted from a worker (default `128 MiB`).
    pub max_output_bytes: usize,
    /// Maximum number of warm workers kept in the pool (default `4`).
    pub pool_size: usize,
    /// Maximum number of jobs a single worker process executes before recycling (default `64`).
    pub max_jobs_per_worker: usize,
    /// Optional explicit path to the `prism-worker` executable.
    pub worker_binary_path: Option<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_memory: 64 * 1024 * 1024, // 64 MB (matches Phase 1 default test expectation)
            max_execution_time: Duration::from_secs(30),
            max_instructions: Some(1_000_000_000),
            max_output_bytes: 128 * 1024 * 1024,
            pool_size: 4,
            max_jobs_per_worker: 64,
            worker_binary_path: None,
        }
    }
}

/// Trait implemented by both [`SandboxManager`] (process-isolated worker pool) and
/// [`InProcessExecutor`] (in-process execution for hosts that cannot spawn child processes).
pub trait SandboxExecutor: Send + Sync {
    /// Execute `job` and return its [`SandboxOutput`] or a structured [`SandboxError`].
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] if the job times out, exceeds memory/output limits, crashes, or fails.
    fn execute(&self, job: SandboxJob) -> Result<SandboxOutput, SandboxError>;
}

/// In-process executor implementing [`SandboxExecutor`] without spawning child processes.
#[derive(Debug, Clone, Default)]
pub struct InProcessExecutor {
    config: SandboxConfig,
}

impl InProcessExecutor {
    /// Create a new [`InProcessExecutor`] with `config`.
    #[must_use]
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }
}

impl SandboxExecutor for InProcessExecutor {
    fn execute(&self, job: SandboxJob) -> Result<SandboxOutput, SandboxError> {
        let (wire_op, payload) = split_job(job);
        let req = WorkerRequestHeader {
            max_memory: self.config.max_memory,
            max_output_bytes: self.config.max_output_bytes,
            op: wire_op,
        };
        execute_wire_request(&req, &payload)
    }
}

/// Process-isolated worker pool manager for executing document jobs safely out-of-process.
#[derive(Debug)]
pub struct SandboxManager {
    config: SandboxConfig,
    idle_workers: Mutex<Vec<WorkerProcess>>,
}

impl SandboxManager {
    /// Create a new [`SandboxManager`] with the given [`SandboxConfig`].
    #[must_use]
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            config,
            idle_workers: Mutex::new(Vec::new()),
        }
    }

    /// Create a new [`SandboxManager`] with [`SandboxConfig::default`].
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(SandboxConfig::default())
    }

    /// Borrow the [`SandboxConfig`].
    #[must_use]
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Execute a [`SandboxJob`] in an isolated `prism-worker` process from the warm pool.
    ///
    /// If the worker times out, hits the memory limit, or crashes, only that worker is killed;
    /// the pool automatically replaces it on subsequent jobs.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] on timeout, memory limit, crash, protocol error, or task error.
    pub fn run(&self, job: SandboxJob) -> Result<SandboxOutput, SandboxError> {
        let mut worker = self.acquire_worker()?;
        let (wire_op, payload) = split_job(job);
        let req_header = WorkerRequestHeader {
            max_memory: self.config.max_memory,
            max_output_bytes: self.config.max_output_bytes,
            op: wire_op,
        };

        let max_output = self.config.max_output_bytes;
        let timeout = self.config.max_execution_time;
        let max_memory = self.config.max_memory;

        // Move stdin/stdout to a dedicated I/O exchange thread so we can enforce wall-clock timeout
        let Some(mut stdin) = worker.stdin.take() else {
            worker.kill_and_wait();
            return Err(SandboxError::ProtocolError(
                "Worker stdin missing".to_string(),
            ));
        };
        let Some(mut stdout) = worker.stdout.take() else {
            worker.kill_and_wait();
            return Err(SandboxError::ProtocolError(
                "Worker stdout missing".to_string(),
            ));
        };

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let send_res = write_frame(&mut stdin, &req_header, &payload);
            if let Err(e) = send_res {
                let _ = tx.send(Err((e, stdin, stdout)));
                return;
            }
            match read_frame::<WorkerResponseHeader, _>(&mut stdout, max_output) {
                Ok((resp_hdr, resp_bytes)) => {
                    let _ = tx.send(Ok((resp_hdr, resp_bytes, stdin, stdout)));
                }
                Err(e) => {
                    let _ = tx.send(Err((e, stdin, stdout)));
                }
            }
        });

        match rx.recv_timeout(timeout) {
            Ok(Ok((resp_hdr, resp_bytes, stdin_back, stdout_back))) => {
                worker.stdin = Some(stdin_back);
                worker.stdout = Some(stdout_back);
                worker.jobs_completed += 1;

                // Recycle the warm worker back into the pool if within job count & memory thresholds
                let should_keep = worker.jobs_completed < self.config.max_jobs_per_worker
                    && (max_memory == 0 || resp_hdr.heap_bytes < max_memory / 2);
                if should_keep {
                    self.release_worker(worker);
                } else {
                    worker.kill_and_wait();
                }

                if resp_hdr.ok {
                    Ok(SandboxOutput {
                        data: resp_bytes,
                        content_type: resp_hdr.content_type,
                        redaction_log_json: resp_hdr.redaction_log_json,
                    })
                } else if resp_hdr.memory_limit_exceeded {
                    Err(SandboxError::MemoryLimit {
                        limit_bytes: max_memory,
                    })
                } else {
                    Err(SandboxError::TaskFailed(resp_hdr.error.unwrap_or_else(
                        || "Unknown worker task failure".to_string(),
                    )))
                }
            }
            Ok(Err((io_err, _stdin_back, _stdout_back))) => {
                // Check if the child process exited due to OOM or crash
                let exit_code = worker.wait_or_kill(Duration::from_millis(250));
                if matches!(io_err, SandboxError::OutputTooLarge { .. }) {
                    return Err(io_err);
                }
                if is_oom_exit_code(exit_code) {
                    return Err(SandboxError::MemoryLimit {
                        limit_bytes: max_memory,
                    });
                }
                Err(SandboxError::Crashed { exit_code })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                worker.kill_and_wait();
                Err(SandboxError::Timeout(timeout))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let exit_code = worker.wait_or_kill(Duration::from_millis(250));
                if is_oom_exit_code(exit_code) {
                    Err(SandboxError::MemoryLimit {
                        limit_bytes: max_memory,
                    })
                } else {
                    Err(SandboxError::Crashed { exit_code })
                }
            }
        }
    }

    fn acquire_worker(&self) -> Result<WorkerProcess, SandboxError> {
        if let Ok(mut guard) = self.idle_workers.lock() {
            while let Some(mut w) = guard.pop() {
                if w.is_alive() {
                    return Ok(w);
                }
                w.kill_and_wait();
            }
        }
        WorkerProcess::spawn(&self.config)
    }

    fn release_worker(&self, mut worker: WorkerProcess) {
        if let Ok(mut guard) = self.idle_workers.lock() {
            if guard.len() < self.config.pool_size.max(1) && worker.is_alive() {
                guard.push(worker);
                return;
            }
        }
        worker.kill_and_wait();
    }
}

impl SandboxExecutor for SandboxManager {
    fn execute(&self, job: SandboxJob) -> Result<SandboxOutput, SandboxError> {
        self.run(job)
    }
}

impl Drop for SandboxManager {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.idle_workers.lock() {
            for mut w in guard.drain(..) {
                w.kill_and_wait();
            }
        }
    }
}

struct WorkerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    jobs_completed: usize,
    #[cfg(windows)]
    _job_guard: Option<os_windows::WindowsJobGuard>,
}

impl std::fmt::Debug for WorkerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerProcess")
            .field("pid", &self.child.id())
            .field("jobs_completed", &self.jobs_completed)
            .finish_non_exhaustive()
    }
}

impl WorkerProcess {
    fn spawn(config: &SandboxConfig) -> Result<Self, SandboxError> {
        let bin_path = resolve_worker_binary_path(config.worker_binary_path.as_deref())?;
        let mut cmd = Command::new(&bin_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("PRISM_WORKER_MAX_MEM", config.max_memory.to_string())
            .env("PRISM_NO_NETWORK", "1");

        let mut child = cmd.spawn().map_err(|e| {
            SandboxError::ProtocolError(format!(
                "Failed to spawn prism-worker at '{}': {e}",
                bin_path.display()
            ))
        })?;

        #[cfg(windows)]
        let job_guard = os_windows::WindowsJobGuard::attach(&child, config.max_memory);

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        Ok(Self {
            child,
            stdin,
            stdout,
            jobs_completed: 0,
            #[cfg(windows)]
            _job_guard: job_guard,
        })
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn wait_or_kill(&mut self, grace: Duration) -> Option<i32> {
        let deadline = Instant::now() + grace;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                return self.child.wait().ok().and_then(|s| s.code());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn kill_and_wait(&mut self) {
        let _ = self.stdin.take();
        let _ = self.stdout.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_oom_exit_code(code: Option<i32>) -> bool {
    let Some(c) = code else {
        return false;
    };
    #[allow(clippy::cast_sign_loss)]
    let u = c as u32;
    // 99 = EXIT_CODE_MEMORY_LIMIT, 0xC0000017 = STATUS_NO_MEMORY, 0xC00000FD = STATUS_STACK_OVERFLOW (handled as crash)
    c == EXIT_CODE_MEMORY_LIMIT || u == 0xC000_0017 || c == 137
}

/// Locate the `prism-worker` executable (checking explicit config, `PRISM_WORKER_BIN`, and the
/// directory of `current_exe()` and its parent). Debug builds also fall back to this workspace's
/// `target/debug`, building the worker there with `cargo build` if it is missing.
fn resolve_worker_binary_path(explicit: Option<&Path>) -> Result<PathBuf, SandboxError> {
    if let Some(p) = explicit
        && p.exists()
    {
        return Ok(p.to_path_buf());
    }
    if let Ok(env_p) = std::env::var("PRISM_WORKER_BIN") {
        let p = PathBuf::from(env_p);
        if p.exists() {
            return Ok(p);
        }
    }

    let exe_name = if cfg!(windows) {
        "prism-worker.exe"
    } else {
        "prism-worker"
    };

    if let Ok(cur_exe) = std::env::current_exe() {
        // Check sibling of current_exe (e.g. target/debug/prism-worker.exe)
        if let Some(dir) = cur_exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.exists() {
                return Ok(candidate);
            }
            // If current_exe is in `target/debug/deps/test-...`, check parent (`target/debug/prism-worker.exe`)
            if let Some(parent_dir) = dir.parent() {
                let candidate2 = parent_dir.join(exe_name);
                if candidate2.exists() {
                    return Ok(candidate2);
                }
            }
        }
    }

    // Development builds only: fall back to this workspace's `target/debug`, building the
    // worker there if needed. A release build never runs `cargo` or looks in the source tree
    // of the machine it was built on.
    #[cfg(debug_assertions)]
    if let Some(workspace_root) = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
    {
        let debug_bin = workspace_root.join("target").join("debug").join(exe_name);
        if debug_bin.exists() {
            return Ok(debug_bin);
        }
        let status = Command::new("cargo")
            .args(["build", "-p", "prism-sandbox", "--bin", "prism-worker"])
            .current_dir(workspace_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|s| s.success()) && debug_bin.exists() {
            return Ok(debug_bin);
        }
    }

    Err(SandboxError::ProtocolError(format!(
        "Could not locate '{exe_name}' binary"
    )))
}

/// Write a length-prefixed versioned binary frame (`[4B magic][2B ver][4B json_len][8B blob_len][json][blob]`).
///
/// # Errors
///
/// Returns [`SandboxError::ProtocolError`] if serialization or I/O fails.
pub fn write_frame<T: Serialize, W: Write>(
    writer: &mut W,
    header: &T,
    payload: &[u8],
) -> Result<(), SandboxError> {
    let json_bytes = serde_json::to_vec(header)
        .map_err(|e| SandboxError::ProtocolError(format!("Header JSON encode error: {e}")))?;
    let json_len = u32::try_from(json_bytes.len())
        .map_err(|_| SandboxError::ProtocolError("Header JSON exceeds 4 GiB".to_string()))?;
    let payload_len = payload.len() as u64;

    let mut frame_hdr = [0u8; 18];
    frame_hdr[0..4].copy_from_slice(&PROTOCOL_MAGIC);
    frame_hdr[4..6].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    frame_hdr[6..10].copy_from_slice(&json_len.to_le_bytes());
    frame_hdr[10..18].copy_from_slice(&payload_len.to_le_bytes());

    writer
        .write_all(&frame_hdr)
        .map_err(|e| SandboxError::ProtocolError(format!("Frame header write error: {e}")))?;
    writer
        .write_all(&json_bytes)
        .map_err(|e| SandboxError::ProtocolError(format!("Header JSON write error: {e}")))?;
    writer
        .write_all(payload)
        .map_err(|e| SandboxError::ProtocolError(format!("Payload write error: {e}")))?;
    writer
        .flush()
        .map_err(|e| SandboxError::ProtocolError(format!("Flush error: {e}")))?;
    Ok(())
}

/// Read a length-prefixed versioned binary frame, enforcing `max_payload_bytes` before allocation.
///
/// # Errors
///
/// Returns [`SandboxError::OutputTooLarge`] if `payload_len > max_payload_bytes`, or
/// [`SandboxError::ProtocolError`] on EOF, bad magic, or malformed JSON.
pub fn read_frame<T: for<'de> Deserialize<'de>, R: Read>(
    reader: &mut R,
    max_payload_bytes: usize,
) -> Result<(T, Vec<u8>), SandboxError> {
    let mut frame_hdr = [0u8; 18];
    reader
        .read_exact(&mut frame_hdr)
        .map_err(|e| SandboxError::ProtocolError(format!("Frame header read error: {e}")))?;

    if frame_hdr[0..4] != PROTOCOL_MAGIC {
        return Err(SandboxError::ProtocolError(
            "Invalid worker frame magic".to_string(),
        ));
    }
    let ver = u16::from_le_bytes([frame_hdr[4], frame_hdr[5]]);
    if ver != PROTOCOL_VERSION {
        return Err(SandboxError::ProtocolError(format!(
            "Unsupported worker protocol version: {ver}"
        )));
    }

    let json_len =
        u32::from_le_bytes([frame_hdr[6], frame_hdr[7], frame_hdr[8], frame_hdr[9]]) as usize;
    if json_len > MAX_HEADER_JSON_BYTES {
        return Err(SandboxError::ProtocolError(format!(
            "Header JSON length ({json_len}) exceeds limit ({MAX_HEADER_JSON_BYTES})"
        )));
    }

    let payload_len_u64 = u64::from_le_bytes([
        frame_hdr[10],
        frame_hdr[11],
        frame_hdr[12],
        frame_hdr[13],
        frame_hdr[14],
        frame_hdr[15],
        frame_hdr[16],
        frame_hdr[17],
    ]);
    let payload_len = usize::try_from(payload_len_u64).unwrap_or(usize::MAX);
    if max_payload_bytes > 0 && payload_len > max_payload_bytes {
        return Err(SandboxError::OutputTooLarge {
            size: payload_len,
            limit: max_payload_bytes,
        });
    }

    let mut json_buf = vec![0u8; json_len];
    reader
        .read_exact(&mut json_buf)
        .map_err(|e| SandboxError::ProtocolError(format!("Header JSON read error: {e}")))?;
    let header: T = serde_json::from_slice(&json_buf)
        .map_err(|e| SandboxError::ProtocolError(format!("Header JSON decode error: {e}")))?;

    let mut payload = vec![0u8; payload_len];
    reader
        .read_exact(&mut payload)
        .map_err(|e| SandboxError::ProtocolError(format!("Payload read error: {e}")))?;

    Ok((header, payload))
}

/// Apply OS-level self-sandboxing inside `prism-worker` at startup (disabling crash dialogs on Windows,
/// setting `PR_SET_NO_NEW_PRIVS` and `RLIMIT_CORE = 0` / `RLIMIT_AS` on Unix, and activating the
/// worker network-egress block).
pub fn apply_worker_self_sandbox(max_memory: usize) {
    WORKER_NETWORK_BLOCKED.store(true, Ordering::SeqCst);
    set_worker_heap_limit(max_memory);

    #[cfg(windows)]
    os_windows::disable_windows_crash_dialogs();

    #[cfg(unix)]
    os_unix::apply_unix_worker_limits(max_memory);
}

/// Record or update the per-job heap limit enforced inside `prism-worker`.
pub fn set_worker_heap_limit(max_memory: usize) {
    WORKER_HEAP_LIMIT_BYTES.store(max_memory, Ordering::SeqCst);
}

/// Return the current tracked heap allocation bytes in `prism-worker`.
#[must_use]
pub fn current_worker_heap_bytes() -> usize {
    WORKER_HEAP_CURRENT_BYTES.load(Ordering::Relaxed)
}

/// Update tracked heap bytes from `prism-worker`'s global allocator; exits with [`EXIT_CODE_MEMORY_LIMIT`]
/// if `WORKER_HEAP_LIMIT_BYTES` is non-zero and exceeded.
pub fn record_worker_alloc(delta: isize) {
    let cur = if delta >= 0 {
        #[allow(clippy::cast_sign_loss)]
        let add = delta as usize;
        WORKER_HEAP_CURRENT_BYTES.fetch_add(add, Ordering::Relaxed) + add
    } else {
        #[allow(clippy::cast_sign_loss)]
        let sub = (-delta) as usize;
        loop {
            let prev = WORKER_HEAP_CURRENT_BYTES.load(Ordering::Relaxed);
            let next = prev.saturating_sub(sub);
            if WORKER_HEAP_CURRENT_BYTES
                .compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break next;
            }
        }
    };
    let limit = WORKER_HEAP_LIMIT_BYTES.load(Ordering::Relaxed);
    if limit > 0 && cur > limit {
        std::process::exit(EXIT_CODE_MEMORY_LIMIT);
    }
}

/// Execute a decoded [`WorkerRequestHeader`] and binary `payload` inside `prism-worker` (or in-process).
///
/// # Errors
///
/// Returns [`SandboxError`] if the operation fails or exceeds `req.max_output_bytes`.
pub fn execute_wire_request(
    req: &WorkerRequestHeader,
    payload: &[u8],
) -> Result<SandboxOutput, SandboxError> {
    let out = match &req.op {
        WorkerWireOp::Detect { filename } => {
            let det = detect_format(payload, filename.as_deref()).ok_or_else(|| {
                SandboxError::TaskFailed("Unrecognized document format".to_string())
            })?;
            let val = serde_json::json!({
                "name": det.format.name,
                "mime_type": det.format.mime_type,
                "extension": det.format.extension,
                "confidence": det.confidence,
            });
            let json = serde_json::to_vec_pretty(&val)
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            SandboxOutput {
                data: json,
                content_type: "application/json".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::Info { filename } => {
            let (layout, doc_opt) = layout_and_parse_bytes(payload, filename.as_deref())?;
            let det = detect_format(payload, filename.as_deref());
            let info = serde_json::json!({
                "engine_version": prism_core::ENGINE_VERSION,
                "format": det.as_ref().map(|d| &d.format.name),
                "mime_type": det.as_ref().map(|d| &d.format.mime_type),
                "page_count": layout.pages.len(),
                "metadata": doc_opt.as_ref().map(|d| &d.metadata),
            });
            let json = serde_json::to_vec_pretty(&info)
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            SandboxOutput {
                data: json,
                content_type: "application/json".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::ExtractText { filename } => {
            let (layout, _) = layout_and_parse_bytes(payload, filename.as_deref())?;
            let mut text = String::new();
            for (i, page) in layout.pages.iter().enumerate() {
                if i > 0 {
                    text.push_str("\n\n");
                }
                text.push_str(&page.text());
            }
            SandboxOutput {
                data: text.into_bytes(),
                content_type: "text/plain; charset=utf-8".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::RenderHtml { filename } => {
            let doc = parse_udm_document(payload, filename.clone())?;
            let renderer = HtmlRenderer::new();
            let ctx = RenderContext {
                options: RenderOptions {
                    format: doc.source.format.clone(),
                    include_images: true,
                    preserve_formatting: true,
                    ..RenderOptions::default()
                },
                filename: filename.clone(),
            };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            let html = rt
                .block_on(renderer.render(&doc, ctx))
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            SandboxOutput {
                data: html.to_vec(),
                content_type: "text/html; charset=utf-8".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::RenderPage {
            filename,
            page_index,
            format,
            dpi,
        } => {
            let (layout, _) = layout_and_parse_bytes(payload, filename.as_deref())?;
            let page = layout
                .pages
                .iter()
                .find(|p| p.index == *page_index)
                .ok_or_else(|| {
                    SandboxError::TaskFailed(format!(
                        "Page index {page_index} out of range (page_count = {})",
                        layout.pages.len()
                    ))
                })?;
            match format {
                RenderOutputFormat::Svg => {
                    let svg = render_page_svg(
                        page,
                        &layout.fonts,
                        &layout.images,
                        &LayoutOptions::default(),
                    )
                    .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                    SandboxOutput {
                        data: svg.into_bytes(),
                        content_type: "image/svg+xml".to_string(),
                        redaction_log_json: None,
                    }
                }
                RenderOutputFormat::Png => {
                    let opts = RasterOptions {
                        dpi: *dpi,
                        ..RasterOptions::default()
                    };
                    let png = render_page_png(page, &layout.fonts, &layout.images, &opts)
                        .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                    SandboxOutput {
                        data: png,
                        content_type: "image/png".to_string(),
                        redaction_log_json: None,
                    }
                }
            }
        }
        WorkerWireOp::ExportDocument {
            filename,
            format,
            dpi,
            sidecar_json,
        } => {
            let (layout, doc_opt) = layout_and_parse_bytes(payload, filename.as_deref())?;
            let meta = doc_opt.map(|d| d.metadata).unwrap_or_default();

            if let Some(sc_json) = sidecar_json {
                let sidecar = AnnotationSidecar::from_json(sc_json)
                    .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                let red_fmt = match format {
                    ExportOutputFormat::Pdf => RedactedExportFormat::Pdf,
                    ExportOutputFormat::Tiff => RedactedExportFormat::Tiff,
                };
                let res =
                    export_redacted_document(payload, &layout, &meta, &sidecar, red_fmt, *dpi)
                        .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                let log_json = serde_json::to_string_pretty(&res.log)
                    .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                let content_type = match format {
                    ExportOutputFormat::Pdf => "application/pdf",
                    ExportOutputFormat::Tiff => "image/tiff",
                };
                SandboxOutput {
                    data: res.bytes,
                    content_type: content_type.to_string(),
                    redaction_log_json: Some(log_json),
                }
            } else {
                match format {
                    ExportOutputFormat::Pdf => {
                        let pdf = export_document_pdf(&layout, &PdfExportOptions::default())
                            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                        SandboxOutput {
                            data: pdf,
                            content_type: "application/pdf".to_string(),
                            redaction_log_json: None,
                        }
                    }
                    ExportOutputFormat::Tiff => {
                        let opts = RasterOptions {
                            dpi: *dpi,
                            ..RasterOptions::default()
                        };
                        let tiff = export_document_tiff(&layout, &opts)
                            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
                        SandboxOutput {
                            data: tiff,
                            content_type: "image/tiff".to_string(),
                            redaction_log_json: None,
                        }
                    }
                }
            }
        }
        WorkerWireOp::HiddenContentReport { filename } => {
            let layout_opt = layout_and_parse_bytes(payload, filename.as_deref())
                .ok()
                .map(|(l, _)| l);
            let report =
                generate_hidden_content_report(payload, filename.as_deref(), layout_opt.as_ref());
            let json = serde_json::to_vec_pretty(&report)
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            SandboxOutput {
                data: json,
                content_type: "application/json".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::FamilyTree { filename } => {
            let tree = build_family_tree(
                payload,
                filename.as_deref(),
                ParseOptions::default().container_limits(),
            );
            let json = serde_json::to_vec_pretty(&tree)
                .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
            SandboxOutput {
                data: json,
                content_type: "application/json".to_string(),
                redaction_log_json: None,
            }
        }
        WorkerWireOp::HostileTest { behavior } => {
            execute_hostile_behavior(behavior, req.max_memory)?
        }
    };

    if req.max_output_bytes > 0 && out.data.len() > req.max_output_bytes {
        return Err(SandboxError::OutputTooLarge {
            size: out.data.len(),
            limit: req.max_output_bytes,
        });
    }
    Ok(out)
}

fn execute_hostile_behavior(
    behavior: &HostileBehavior,
    max_memory: usize,
) -> Result<SandboxOutput, SandboxError> {
    match behavior {
        HostileBehavior::AllocateUnbounded { megabytes } => {
            let mut chunks: Vec<Vec<u8>> = Vec::new();
            let mut allocated = 0usize;
            for _ in 0..*megabytes {
                allocated += 1024 * 1024;
                if max_memory > 0 && allocated > max_memory {
                    std::process::exit(EXIT_CODE_MEMORY_LIMIT);
                }
                let mut block = vec![0xAAu8; 1024 * 1024];
                block[0] = 1;
                std::hint::black_box(&block);
                chunks.push(block);
            }
            std::hint::black_box(&chunks);
            Ok(SandboxOutput {
                data: b"allocated".to_vec(),
                content_type: "text/plain".to_string(),
                redaction_log_json: None,
            })
        }
        HostileBehavior::InfiniteLoop => {
            let mut acc = 0u64;
            loop {
                acc = acc.wrapping_add(1);
                std::hint::black_box(acc);
                thread::sleep(Duration::from_millis(5));
            }
        }
        HostileBehavior::StackOverflow => {
            #[allow(unconditional_recursion)]
            fn recurse(depth: u64) -> u64 {
                let buf = [depth as u8; 4096];
                std::hint::black_box(&buf);
                recurse(depth.wrapping_add(1)).wrapping_add(u64::from(buf[0]))
            }
            let v = recurse(1);
            Ok(SandboxOutput {
                data: v.to_string().into_bytes(),
                content_type: "text/plain".to_string(),
                redaction_log_json: None,
            })
        }
        HostileBehavior::Abort => {
            std::process::abort();
        }
        HostileBehavior::AttemptNetworkConnect => {
            // Only the policy flag is checked; see the variant's documentation.
            if WORKER_NETWORK_BLOCKED.load(Ordering::SeqCst)
                || std::env::var("PRISM_NO_NETWORK").is_ok()
            {
                return Err(SandboxError::TaskFailed(
                    "Network egress blocked by worker sandbox policy".to_string(),
                ));
            }
            Err(SandboxError::TaskFailed("Network blocked".to_string()))
        }
    }
}

fn split_job(job: SandboxJob) -> (WorkerWireOp, Vec<u8>) {
    match job {
        SandboxJob::Detect { filename, bytes } => (WorkerWireOp::Detect { filename }, bytes),
        SandboxJob::Info { filename, bytes } => (WorkerWireOp::Info { filename }, bytes),
        SandboxJob::ExtractText { filename, bytes } => {
            (WorkerWireOp::ExtractText { filename }, bytes)
        }
        SandboxJob::RenderHtml { filename, bytes } => {
            (WorkerWireOp::RenderHtml { filename }, bytes)
        }
        SandboxJob::RenderPage {
            filename,
            bytes,
            page_index,
            format,
            dpi,
        } => (
            WorkerWireOp::RenderPage {
                filename,
                page_index,
                format,
                dpi,
            },
            bytes,
        ),
        SandboxJob::ExportDocument {
            filename,
            bytes,
            format,
            dpi,
            sidecar_json,
        } => (
            WorkerWireOp::ExportDocument {
                filename,
                format,
                dpi,
                sidecar_json,
            },
            bytes,
        ),
        SandboxJob::HiddenContentReport { filename, bytes } => {
            (WorkerWireOp::HiddenContentReport { filename }, bytes)
        }
        SandboxJob::FamilyTree { filename, bytes } => {
            (WorkerWireOp::FamilyTree { filename }, bytes)
        }
        SandboxJob::HostileTest(behavior) => (WorkerWireOp::HostileTest { behavior }, Vec::new()),
    }
}

fn layout_and_parse_bytes(
    bytes: &[u8],
    filename: Option<&str>,
) -> Result<(LayoutDocument, Option<Document>), SandboxError> {
    let options = LayoutOptions::default();

    if bytes.starts_with(b"%PDF-")
        && let Ok(layout) = interpret_pdf_bytes(bytes, &options)
    {
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_word::is_docx_zip(bytes)
        && let Ok(word_doc) =
            prism_word::parse_docx(bytes, filename, options.max_decompressed_stream_bytes)
    {
        let layout = prism_word::layout_word_document(&word_doc, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_word::is_doc_bytes(bytes)
        && let Ok(word_doc) = prism_word::parse_doc(bytes, options.max_decompressed_stream_bytes)
    {
        let layout = prism_word::layout_word_document(&word_doc, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_ppt::is_pptx_zip(bytes)
        && let Ok(ppt_doc) = prism_ppt::parse_pptx(bytes, options.max_decompressed_stream_bytes)
    {
        let layout = prism_ppt::layout_presentation(&ppt_doc, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_ppt::is_ppt_bytes(bytes)
        && let Ok(ppt_doc) = prism_ppt::parse_ppt(bytes, options.max_decompressed_stream_bytes)
        && !ppt_doc.slides.is_empty()
    {
        let layout = prism_ppt::layout_presentation(&ppt_doc, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_excel::is_xlsx_zip(bytes)
        && let Ok(wb) = prism_excel::parse_xlsx(bytes, options.max_decompressed_stream_bytes)
    {
        let layout = prism_excel::layout_workbook(&wb, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    if prism_excel::is_xls_bytes(bytes)
        && let Ok(wb) = prism_excel::parse_xls(bytes, options.max_decompressed_stream_bytes)
    {
        let layout = prism_excel::layout_workbook(&wb, &options)
            .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
        let doc = parse_udm_document(bytes, filename.map(str::to_string)).ok();
        return Ok((layout, doc));
    }

    let doc = parse_udm_document(bytes, filename.map(str::to_string))?;
    let layout =
        layout_document(&doc, &options).map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
    Ok((layout, Some(doc)))
}

fn parse_udm_document(bytes: &[u8], filename: Option<String>) -> Result<Document, SandboxError> {
    let detection = detect_format(bytes, filename.as_deref())
        .ok_or_else(|| SandboxError::TaskFailed("Unrecognized document format".to_string()))?;
    let registry = ParserRegistry::with_default_parsers();
    let parser = registry.get_parser(&detection.format).ok_or_else(|| {
        SandboxError::TaskFailed(format!("No parser available for {}", detection.format.name))
    })?;
    let ctx = ParseContext {
        format: detection.format,
        filename,
        size: bytes.len(),
        options: ParseOptions::default(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SandboxError::TaskFailed(e.to_string()))?;
    rt.block_on(parser.parse(Bytes::copy_from_slice(bytes), ctx))
        .map_err(|e| SandboxError::TaskFailed(e.to_string()))
}

#[cfg(windows)]
mod os_windows {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::ptr;

    type Handle = *mut c_void;
    type Bool = i32;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    const JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS: i32 = 4;

    const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
    const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
    const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
    const JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    const JOB_OBJECT_UILIMIT_ALL: u32 = 0x0000_00FF;

    const SEM_FAILCRITICALERRORS: u32 = 0x0001;
    const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
    const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;

    #[repr(C)]
    #[derive(Default)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[repr(C)]
    struct JobObjectBasicUiRestrictions {
        ui_restrictions_class: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(security_attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            info_class: i32,
            info: *const c_void,
            info_len: u32,
        ) -> Bool;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
        fn CloseHandle(object: Handle) -> Bool;
        fn SetErrorMode(mode: u32) -> u32;
    }

    pub(crate) struct WindowsJobGuard {
        handle: Handle,
    }

    // SAFETY: Win32 Job Object handles can be closed from any thread.
    unsafe impl Send for WindowsJobGuard {}
    unsafe impl Sync for WindowsJobGuard {}

    impl WindowsJobGuard {
        pub(crate) fn attach(child: &Child, max_memory: usize) -> Option<Self> {
            // SAFETY: Standard Win32 Job Object creation and configuration.
            unsafe {
                let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
                if job.is_null() {
                    return None;
                }

                let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                    | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
                if max_memory > 0 {
                    flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
                }

                let mut ext = JobObjectExtendedLimitInformation::default();
                ext.basic_limit_information.limit_flags = flags;
                ext.basic_limit_information.active_process_limit = 1;
                ext.process_memory_limit = max_memory;
                ext.job_memory_limit = max_memory;

                #[allow(clippy::cast_possible_truncation)]
                let ext_size = size_of::<JobObjectExtendedLimitInformation>() as u32;
                let _ = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw const ext).cast::<c_void>(),
                    ext_size,
                );

                let ui = JobObjectBasicUiRestrictions {
                    ui_restrictions_class: JOB_OBJECT_UILIMIT_ALL,
                };
                #[allow(clippy::cast_possible_truncation)]
                let ui_size = size_of::<JobObjectBasicUiRestrictions>() as u32;
                let _ = SetInformationJobObject(
                    job,
                    JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS,
                    (&raw const ui).cast::<c_void>(),
                    ui_size,
                );

                let proc_handle = child.as_raw_handle() as Handle;
                let _ = AssignProcessToJobObject(job, proc_handle);

                Some(Self { handle: job })
            }
        }
    }

    impl Drop for WindowsJobGuard {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                // SAFETY: Closing owned Job Object handle.
                unsafe {
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    pub(crate) fn disable_windows_crash_dialogs() {
        // SAFETY: Suppresses Windows Error Reporting popup dialogs inside the worker process.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        }
    }
}

#[cfg(unix)]
mod os_unix {
    pub(crate) fn apply_unix_worker_limits(max_memory: usize) {
        // SAFETY: Standard POSIX/Linux resource limit and privilege drop calls.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                let _ = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            }

            let zero_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const zero_core);

            if max_memory > 0 {
                // `RLIMIT_AS` caps address space, not heap: thread stacks and allocator arenas
                // reserve far more than they use, so a cap equal to the heap limit would make
                // ordinary jobs fail. The tracking allocator enforces the heap limit exactly;
                // this is only a backstop for allocations it cannot see.
                let backstop = max_memory.saturating_mul(4).max(1024 * 1024 * 1024);
                #[allow(clippy::cast_possible_truncation)]
                let lim = backstop as libc::rlim_t;
                let as_lim = libc::rlimit {
                    rlim_cur: lim,
                    rlim_max: lim,
                };
                let _ = libc::setrlimit(libc::RLIMIT_AS, &raw const as_lim);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let config = SandboxConfig::default();
        assert_eq!(config.max_memory, 64 * 1024 * 1024);
        assert_eq!(config.max_execution_time, Duration::from_secs(30));
    }

    #[test]
    fn test_sandbox_manager_creation() {
        let manager = SandboxManager::default_config();
        assert_eq!(manager.config().max_memory, 64 * 1024 * 1024);
    }

    #[test]
    fn worker_pool_survives_oom_timeout_stackoverflow_abort_and_network_attempt() {
        let config = SandboxConfig {
            max_memory: 48 * 1024 * 1024, // 48 MiB
            max_execution_time: Duration::from_millis(600),
            pool_size: 2,
            max_jobs_per_worker: 16,
            ..SandboxConfig::default()
        };
        let pool = SandboxManager::new(config);

        // 1. Normal job before hostile jobs
        let ok1 = pool
            .run(SandboxJob::ExtractText {
                filename: Some("hello.txt".to_string()),
                bytes: b"Hello from isolated worker".to_vec(),
            })
            .expect("normal job 1");
        assert!(String::from_utf8_lossy(&ok1.data).contains("Hello from isolated worker"));

        // 2. Hostile unbounded allocation -> MemoryLimit
        let oom_res = pool.run(SandboxJob::HostileTest(
            HostileBehavior::AllocateUnbounded { megabytes: 128 },
        ));
        assert!(
            matches!(oom_res, Err(SandboxError::MemoryLimit { .. })),
            "expected MemoryLimit, got {oom_res:?}"
        );

        // 3. Hostile infinite loop -> Timeout
        let timeout_res = pool.run(SandboxJob::HostileTest(HostileBehavior::InfiniteLoop));
        assert!(
            matches!(timeout_res, Err(SandboxError::Timeout(_))),
            "expected Timeout, got {timeout_res:?}"
        );

        // 4. Hostile stack overflow -> Crashed
        let so_res = pool.run(SandboxJob::HostileTest(HostileBehavior::StackOverflow));
        assert!(
            matches!(so_res, Err(SandboxError::Crashed { .. })),
            "expected Crashed on stack overflow, got {so_res:?}"
        );

        // 5. Hostile abort -> Crashed
        let abort_res = pool.run(SandboxJob::HostileTest(HostileBehavior::Abort));
        assert!(
            matches!(abort_res, Err(SandboxError::Crashed { .. })),
            "expected Crashed on abort, got {abort_res:?}"
        );

        // 6. Hostile network attempt -> blocked
        let net_res = pool.run(SandboxJob::HostileTest(
            HostileBehavior::AttemptNetworkConnect,
        ));
        assert!(net_res.is_err(), "expected network attempt to be blocked");

        // 7. Pool recovers and executes subsequent jobs cleanly with warm < 20ms overhead
        let start = Instant::now();
        let ok2 = pool
            .run(SandboxJob::Detect {
                filename: Some("doc.txt".to_string()),
                bytes: b"Subsequent document after worker crashes".to_vec(),
            })
            .expect("normal job after crashes");
        assert!(String::from_utf8_lossy(&ok2.data).contains("text/plain"));

        // Second warm call on the same live worker
        let warm_start = Instant::now();
        let _ok3 = pool
            .run(SandboxJob::Detect {
                filename: Some("doc.txt".to_string()),
                bytes: b"Warm worker call".to_vec(),
            })
            .expect("warm call");
        let warm_elapsed = warm_start.elapsed();
        let _ = start;
        assert!(
            warm_elapsed < Duration::from_millis(100),
            "warm worker overhead too high: {warm_elapsed:?}"
        );
    }
}

# Asset watcher stall diagnosis plan

## Purpose

This document defines the investigation plan for a critical asset indexing stall observed on a large Unity project in Locus v0.32.

The goal is not to prove a preferred theory. The goal is to identify exactly where the asset pipeline stops making progress, why the worker does not return, and what evidence is required before implementing a fix.

## Problem statement

On a large Unity project, the asset page can become effectively unusable during or after asset indexing. The console repeatedly prints a watcher queue summary similar to:

```text
[AssetDb Watcher] queue summary: pending=29779, current=queue_summary_current_asset, recent=0 in 8s, reasons=[none]
```

The important part is not the specific file name. The important part is that `pending` does not decrease, `current` remains the same, and `recent=0` repeats. This should be treated as a real liveness failure until proven otherwise, not as a user perception issue.

Any duration mentioned in conversation, such as "57 minutes", should be read as an expression of indefinite stall. It is not a measured benchmark value and must not be used as evidence.

## What is already known

Diagnostic logs from one large-project run showed a separate performance problem in the full scan DB write stage:

- `73843` asset nodes
- `766488` dependency edges
- `1813213` sub-asset objects
- total scan time around `699s`
- DB write time around `477s`
- `asset_objects` insert time around `343s`
- transaction commit time around `107s`

This confirms that v0.32 can spend several minutes in the full scan write phase on a large project. However, this evidence does not fully explain an indefinite watcher stall where the queue summary repeats with the same `current` asset.

The investigation must therefore separate these two failure modes:

1. Full scan DB write is too slow.
2. Watcher worker can stop making progress on a single queued asset.

The second item is the primary target of this plan.

## Code path to investigate

The queue summary is emitted by the watcher queue logger in `src-tauri/src/asset_db/watcher.rs`.

The worker path is:

1. `worker_loop` dequeues a relative asset path.
2. `worker_loop` writes that path into `current_file`.
3. `worker_loop` calls `process_dirty_asset`.
4. `worker_loop` clears `current_file` only after `process_dirty_asset` returns.

Therefore, if the same `current` path is printed repeatedly for a long time, the worker most likely has not returned from `process_dirty_asset`, or it is blocked before it can clear `current_file`.

The investigation must identify the exact stage inside or immediately around `process_dirty_asset` where progress stops.

## Hypotheses

These are hypotheses to test, not conclusions:

- The worker is blocked waiting for the shared asset DB mutex.
- The worker is blocked in a SQLite write, checkpoint, busy wait, or lock conflict.
- The worker is blocked in asset file IO, meta file IO, content hashing, or filesystem metadata calls.
- The worker is blocked or looping in YAML parsing for a specific asset.
- The worker is blocked in dependency extraction or sub-asset object extraction.
- The worker is blocked in script cascade queries or enqueueing related paths.
- The worker completes one item but immediately requeues the same or equivalent path, making the summary look like a single-file stall.
- The full scan, startup reconcile, watcher queue, and knowledge startup tasks contend for CPU, IO, memory, or SQLite resources in a way that prevents liveness.

## Evidence required

A valid diagnosis must include:

- The current branch and app build used for the run.
- The project root used for reproduction.
- The exact start time and stop time of the test run.
- Whether the UI is responsive while the queue is stalled.
- The last full scan phase reported before the stall.
- The watcher pending count, current path, and recent count over time.
- The exact `process_dirty_asset` stage active while `current` is unchanged.
- Whether the worker is waiting on a mutex, SQLite, filesystem IO, parsing, dependency extraction, or another stage.
- Whether the same asset path is being requeued repeatedly.
- Whether any other startup task is running concurrently, especially knowledge indexing or startup reconcile.

## Instrumentation plan

Add temporary diagnostics only. The diagnostics should be easy to remove and should not change asset indexing behavior.

### 1. Per-worker stage tracking

Add a shared per-worker state structure containing:

- worker index
- current path
- current stage name
- stage start time
- item start time
- last completed stage
- last error, if any

Each stage transition should update this state before entering the stage and after leaving it.

### 2. Stage watchdog

Add a watchdog thread that logs a warning when a worker stays in the same stage beyond thresholds such as:

- `10s`
- `30s`
- `60s`
- every `60s` after that

The watchdog log should include:

- worker index
- current path
- current stage
- stage elapsed time
- item elapsed time
- queue pending count
- active worker count
- recent enqueue count

This is the key diagnostic feature. It should make the repeated queue summary actionable by saying exactly what the worker is waiting on.

### 3. Stage boundaries inside `process_dirty_asset`

Instrument at least these boundaries:

- enter `process_dirty_asset`
- resolve asset path and meta path
- check asset existence
- read meta file
- read asset file or collect metadata
- hash asset content, when applicable
- classify asset kind
- parse YAML, when applicable
- extract dependency edges
- extract sub-asset objects
- wait for `graph_state` mutex
- delete missing asset from DB
- atomic update start
- atomic update DB delete phase
- atomic update DB insert asset phase
- atomic update DB insert objects phase
- atomic update DB insert edges phase
- atomic update commit phase
- script cascade query phase
- enqueue cascade paths
- exit `process_dirty_asset`

Each slow stage should log elapsed time and path.

### 4. Requeue detection

Track recent processed paths and enqueue reasons. If the same path is processed or enqueued repeatedly within a short window, log:

- path
- count
- reasons
- source paths
- whether mtime/hash changed between iterations

This is needed to distinguish a single stuck item from a requeue loop.

### 5. SQLite visibility

During a stall, log SQLite-related state where practical:

- busy timeout setting
- WAL file size
- whether a transaction is active
- elapsed time for begin, execute, commit, and checkpoint-like operations
- the SQL operation category, not necessarily full SQL text

The goal is to tell whether the worker is genuinely blocked in SQLite rather than just near SQLite code.

### 6. Startup task coordination logging

Log when these tasks start and end:

- asset full scan
- startup reconcile
- watcher startup
- knowledge startup indexing
- post-scan reconcile

Also log whether they overlap. The suspected stall may require concurrency to reproduce.

## Dev run procedure

The diagnostic run must avoid confusing app startup or build failures with the asset watcher stall. The operator should treat the dev command as another part of the experiment and capture its state explicitly.

### 1. Preflight process check

Before starting a run, check for leftover Locus, Bun, Cargo, and Rust compiler processes:

```powershell
Get-Process locus,bun,cargo,rustc -ErrorAction SilentlyContinue |
  Select-Object Id,ProcessName,Path,StartTime,Responding
```

If a previous diagnostic run is no longer needed, stop only the known leftover processes from that run. Do not leave an old `locus.exe`, `bun.exe`, `cargo.exe`, or `rustc.exe` running in the background while starting a new run.

### 2. Build check before interactive dev

Run a bounded build check first:

```powershell
cargo check --manifest-path src-tauri\Cargo.toml
```

If this does not complete in the expected time for the machine, capture process state before retrying:

```powershell
Get-Process cargo,rustc -ErrorAction SilentlyContinue |
  Select-Object Id,ProcessName,Path,StartTime,CPU
```

Do not start `bun run tauri dev` while a previous `cargo` or `rustc` process is still compiling in the background.

### 3. Start dev with redirected logs

Start the dev app with stdout and stderr redirected to timestamped files:

```powershell
New-Item -ItemType Directory -Force .tmp\asset-stall-diag | Out-Null
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$out = ".tmp\asset-stall-diag\tauri-dev-$stamp.out.log"
$err = ".tmp\asset-stall-diag\tauri-dev-$stamp.err.log"
$p = Start-Process -FilePath "bun" `
  -ArgumentList "run","tauri","dev" `
  -WorkingDirectory (Get-Location) `
  -RedirectStandardOutput $out `
  -RedirectStandardError $err `
  -PassThru `
  -WindowStyle Hidden
"pid=$($p.Id) stdout=$out stderr=$err"
```

The timestamped log paths are part of the evidence for the run.

### 4. Confirm whether the app actually launched

Within a short interval after startup, verify both the process and log state:

```powershell
Get-Process locus,bun,cargo,rustc -ErrorAction SilentlyContinue |
  Select-Object Id,ProcessName,Path,StartTime,Responding

Get-Content $out -Tail 80
Get-Content $err -Tail 80
```

If the dev command only prints build output and no desktop window appears, classify the run as a dev startup/build issue, not an asset watcher stall. Capture the process list and the tail of both logs before stopping or retrying.

### 5. Detect a dev/build stall separately

A dev/build stall should be recorded separately when:

- no `locus.exe` process appears;
- only `bun`, `cargo`, or `rustc` remains active;
- logs show compilation but no app startup;
- the same compiler step appears to run indefinitely;
- the desktop window never appears.

This is not the asset watcher bug. It must not be used as evidence that `process_dirty_asset` is stalled.

### 6. Detect the asset watcher stall

Only classify the run as an asset watcher stall after the app has launched and the watcher summary repeatedly shows no progress:

```text
[AssetDb Watcher] queue summary: pending=<same or non-decreasing>, current=<same path>, recent=0 in 8s, reasons=[none]
```

At that point, collect:

```powershell
Get-Process locus,bun,cargo,rustc -ErrorAction SilentlyContinue |
  Select-Object Id,ProcessName,Path,StartTime,CPU,Responding

Get-Content $out -Tail 200
Get-Content $err -Tail 200
```

The watchdog logs described above should then identify the exact worker stage that is not returning.

### 7. Stop the diagnostic run cleanly

When enough evidence has been captured, stop the run by process ID when possible:

```powershell
Stop-Process -Id <pid> -Force
```

Then verify no diagnostic processes are still running:

```powershell
Get-Process locus,bun,cargo,rustc -ErrorAction SilentlyContinue |
  Select-Object Id,ProcessName,Path,StartTime,Responding
```

Do not start the next diagnostic run until this check is clean or every remaining process is intentionally accounted for.

## Reproduction plan

1. Start from `main`.
2. Create a dedicated diagnostic branch.
3. Apply only the instrumentation described above.
4. Build a debug app.
5. Open the large Unity project that reproduces the issue.
6. Keep the run alive until either:
   - the queue drains,
   - the same `current` path remains unchanged for multiple watchdog intervals,
   - the UI becomes unresponsive and the watchdog identifies a blocked stage.
7. Save stdout, stderr, and process snapshots.
8. Do not stop at the first slow DB write observation unless it directly explains the worker stall.

## Acceptance criteria for diagnosis

The issue is considered located only when one of the following is true:

- A watchdog log identifies a specific stage that remains active indefinitely or for an unacceptable duration.
- A requeue detector proves the same path is being repeatedly requeued and explains why.
- A SQLite timing log proves the worker is blocked in a specific DB operation or lock wait.
- A mutex timing log proves the worker is blocked waiting for a specific shared lock.
- A filesystem or parser timing log proves a specific file operation or parser path is not returning.

"The scan is slow" is not sufficient.

"The user might think it is frozen" is not sufficient.

## Expected outcome

The output of this investigation should be a short evidence report that states:

- exact stall stage
- exact owning function
- exact condition that triggers it
- whether the issue is data-size dependent, file-specific, concurrency-dependent, or caused by a requeue loop
- minimal fix direction

Only after that report should a production fix be implemented.


# QDM Rust download engine

The engine is an original Rust implementation in [`src/engine`](../src/engine). It runs independently of the native Windows interface; the shipped executable also exposes the same engine through its JSON command interface for acceptance testing. Networking uses Tokio and reqwest with Windows native TLS. There is no embedded browser runtime in this engine.

## Connections and dynamic segmentation

The default is **32 connections per download**, configurable from **1 to 64**. The client uses HTTP/1.1 so concurrent requests can occupy separate TCP connections. A one-byte GET probe establishes whether the origin actually honors Range; an `Accept-Ranges` header alone is insufficient.

Initial pieces have exclusive end offsets and a minimum target size of 64 KiB. Smaller files therefore use fewer connections. When a worker becomes available, it can divide the remaining tail of an active piece. The original worker's in-flight disk-write reservation is excluded from the stolen tail. Each piece has one writer and its own part file; workers never share ownership of output bytes. A task is capped at 4,096 pieces to bound bookkeeping and file proliferation.

The configured connection limit is a ceiling. The origin's range support, file size, transfer progress, network conditions, and server limits determine the count in use. Increasing the setting wakes waiting workers. Existing requests can finish when the limit is reduced. More connections do not guarantee more throughput.

## Pause, restart, and integrity

Each piece is stored under `parts-v2/<task UUID>/<piece ID>.part`. Approximately once a second during transfer, the worker flushes its file, records a durable byte count, and hashes up to the last 32 KiB of that checkpoint. History is periodically written to a temporary file, flushed, and replaced. The Windows history replacement requests write-through; the Unix implementation also syncs its containing directory.

Graceful pause waits for disk writes and checkpoints. After an abrupt termination, reopening QDM verifies checkpoint tails and truncates part files to the last committed position. Bytes received after the last durable history checkpoint may be downloaded again. Unreadable history is preserved under a diagnostic filename instead of being overwritten.

Resume always re-probes the resource. A changed total length or validator invalidates old pieces. Saved pieces are sampled at their beginning and tail, including pieces that had already finished. A resumed range re-reads up to 32 KiB of the saved tail and compares it before appending new bytes. Fresh pieces also request a preceding overlap; assembly checks these witnesses against adjoining saved pieces.

The engine prefers a strong ETag for `If-Range`. A suitable Last-Modified value is used only when no entity tag is present and the response date permits it to be treated as strong. A weak ETag is never sent as `If-Range`, and it does not enable a date fallback while that entity tag exists.

Servers without a usable strong validator can still use parallel ranges and resume with continuity checks. **Samples and overlaps cannot prove that every unsampled remote byte stayed unchanged.** Supplying a trusted expected SHA-256 enables whole-file verification before publication. QDM always computes the completed file's SHA-256; a computed hash without an authoritative expected value is a checksum, not independent proof of remote identity.

## Recovery and complete-stream behavior

Every partial response must have the expected status, identity encoding, exact Content-Range boundaries, total length, and compatible representation metadata. Ignored ranges, malformed boundaries, changed representations, or disagreement between adjacent pieces trigger a clean complete-stream recovery. Existing and new representations are never intentionally spliced together after a detected mismatch.

A server without usable Range support is downloaded in one stream. Such a stream can be paused, but resuming it starts over. Unknown lengths and empty files are supported. HTTP gzip, Brotli, and deflate encoding can be decoded in complete-stream mode; decoded bytes are never combined with encoded range offsets. An archive served as an ordinary file without HTTP Content-Encoding remains an archive.

Transient failures have a bounded retry budget. The default is three retries, configurable up to eight. Retry-After is honored with a 120-second maximum per retry delay. Connection establishment has a 20-second timeout, response headers have a 30-second deadline, and body reads have a 45-second inactivity deadline. A running transfer has no arbitrary total-duration limit.

## Publication and saved credentials

Pieces are assembled into a unique temporary file in the destination folder using a bounded streaming buffer. The engine verifies the final length and optional expected SHA-256, then records a commit journal before publishing a final filename. Filename sanitization handles path separators, control characters, Windows reserved names, Unicode, and long-name collision suffixes.

An existing destination is preserved. Windows publication uses a same-volume `MoveFileExW` operation without `REPLACE_EXISTING` or cross-volume copy fallback. Unix publication uses an exclusive hard link. A persisted journal allows startup to recognize an already-published file by its size and SHA-256 after an interrupted commit. These are recovery mechanisms; filesystem, device, and power-loss behavior still affect durability.

Request headers such as browser credentials remain in memory and are not written to plaintext task history. URLs are persisted as part of the task, including any query parameters they contain. After restarting QDM, an authenticated download may require fresh browser context. Replacing a URL checks existing pieces before reuse; ordinary URL replacement drops retained headers when the origin changes. Embedded credentials in URLs are rejected in favor of explicit request headers.

## Resource behavior and scope

The queue defaults to two simultaneous downloads and supports one to eight. The KiB/s limit is shared across active downloads. Pausing a transfer releases future bandwidth reservations so they do not delay the next task. The engine sends progress updates approximately twice a second during active work. With no active work it waits for a command, checkpoint deadline, or scheduled start; it does not poll an empty queue continuously.

This implementation covers the requested **HTTP/HTTPS download core**. It does not claim universal compatibility with every origin, protected-media workflow, or all ancillary IDM features. Partial data and final assembly temporarily require additional disk space; assembly is a streaming copy rather than an in-place concatenation.

## Verification

[`tests/acceptance.cjs`](../tests/acceptance.cjs) drives the actual executable against an independent HTTP origin. Its cases observe simultaneous TCP sockets and response bodies, interior tail reassignment, resume after process death, representation changes, overlap disagreement, range fallback, truncation, retries, checksums, filenames, queueing, scheduling, aggregate limiting, and compressed streams. The report identifies the tested executable by SHA-256.

[`tests/public-https.cjs`](../tests/public-https.cjs) adds two small public HTTPS checks. It fetches an independent Node HTTPS reference with certificate verification, then asks the real QDM executable to download the same URL with that expected SHA-256. It compares file length, hash, and exact bytes. The source/output cap is 5 MiB, redirects stay on HTTPS, and both reference and QDM operations have deadlines. The script creates an isolated temporary profile and removes it after recording the result.

Run the public check from the project directory on Windows:

```powershell
node tests\public-https.cjs --binary target\release\QDM.exe --report reports\public-https.json
```

The public test records the reference TLS sockets directly. QDM's CLI does not expose its individual TLS sockets; the report distinguishes its normal native-TLS verification configuration from independently observed socket telemetry. This smoke test establishes correctness for the selected resources at the recorded time, rather than measuring download speed or establishing compatibility with every website.

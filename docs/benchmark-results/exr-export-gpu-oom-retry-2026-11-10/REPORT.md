# EXR: export retries a photo whose render ran out of graphics memory (2026-11-10)

Linux PC, Vulkan NVIDIA RTX 3080 (10 GB), release builds, `xvfb-run -a`, scratch HOME, scratch copies of
two public corpus RAWs (pentax-k1.PEF, olympus-em1m3.ORF), recipe `raw.new`, 16-bit TIFF. The app
binary is a QA build (identifier `cc.rawtonic.qa.exr`, feature `qa-control`).

## Injected out-of-memory (`inject.sh`, final build f68dbf96; same result on b897a95b)

`RAWTONIC_TEST_GPU_OOM=<n>` (QA builds only) fails the first n export renders of the process with the GPU
out-of-memory error, after their files are planned. Folder export of both RAWs:

| run | fault | rc | files (SHA-256) |
|---|---|---|---|
| A | none | 0 | `olympus-em1m3_edited.tiff` d9780dfc…, `pentax-k1_edited.tiff` 42c93627… |
| B | first render | 0 | the same two names and hashes (1 injection) |
| C | every render | 1 | none; 8 injections (2 photos × 4 attempts); both reported "Not enough graphics memory for this photo. …" |

So a retried export is byte-identical to a normal one, under the name its first attempt reserved (no `_1`).

## Real out-of-memory (`real.sh`, AXF's `vram_hog.cu`, GPU otherwise idle)

The hog takes all free VRAM but 1200 MiB (held 7945 of 9145 MiB free), one pentax-k1 export starts, the
hog is killed HOLD s after the export's first out-of-memory warning.

| binary | hog | rc | output |
|---|---|---|---|
| app | none | 0 | 42c93627… |
| app | freed 1 s after the OOM | 0 | 42c93627… (one retry after 0.5 s; export processor released) |
| app | held 20 s (longer than the 7.5 s of retries) | 1 | none; 3 retries, then "Not enough graphics memory for this photo. …" |
| CLI | none | 0 | 42c93627… |
| CLI | freed 1 s after the OOM | 0 | 42c93627… (one retry) |

The device survives a real wgpu out-of-memory: the retry in the same process renders the same bytes.

## Byte identity otherwise

`check_host.sh` (notes repo) vs `prv18-base-ff254602` (main's render code): b897a95b CLI 326/326 and app
326/326 byte-identical. The CLI run needed one harness retry: canon-r5.CR3 `raw.effects` failed
"ran out of memory … (render)" with 8.5 of 10 GB used by co-tenant jobs — the CLI defect this change
also fixes.
The final f68dbf96: CLI 326/326 and app 326/326 byte-identical, no retry needed.

Metal not run (coordinator: no shader or render-math change).

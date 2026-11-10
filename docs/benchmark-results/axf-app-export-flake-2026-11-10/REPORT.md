# AXF: transient app-export failures on the Linux PC (2026-11-10)

**Result: cause found, not an app defect in the render path.** The app export fails when
another process leaves the GPU too little free memory. Reproduced on demand; not caused by
xvfb or by the harness' parallelism. Linux PC, Vulkan NVIDIA RTX 3080 (10 GB), app binary of
`prv14-base-e57dfdaa`, all runs under `xvfb-run -a`, QA identifier (`cc.rawtonic.editor` home is a scratch HOME).

## Evidence

1. **No flake without a co-tenant.** 120 exports of `pentax-k1.PEF` (`raw.detail`, `raw.new`;
   2 parallel loops) and 672 exports of pentax + olympus × all 28 `raw.*` recipes (2 jobs, 12 rounds,
   stderr kept): 0 failures, every recipe the same hash in every round (792 exports).
2. **Out of memory reproduces it.** `vram_hog.cu` takes all free VRAM except N MiB. One `pentax-k1.PEF`
   `raw.new` export with N MiB left free (9145 MiB free before):

   | left free | result |
   |---|---|
   | 3000 | ok (23 s) |
   | 1800 | exit 1, no output: `GPU render failed: Out of Memory` / "The graphics card ran out of memory for this photograph" |
   | 1200 | same |
   | 800 | exit 1: `The warp failed on the GPU: Out of Memory`, "Export completed with 1 errors." |

   So a 36 MP export needs between 1.8 and 3 GB free; the failure is exit 1 with no file, exactly what the
   harness recorded as `FAIL`. The app already maps GPU out-of-memory to that message (`gpu_scoped`).
3. **Co-tenants existed.** Other workers' GPU tests ran on the same card during the preservation runs
   (notes `preservation-run.md` PRV14: "a second worker's GPU test held the build lock ~70 min"; kernel
   log: 36 `NVRM` GPU timeouts at 2026-11-09 21:59:57, inside the PRV14 app baseline run 21:47–22:29, and
   `pipeline-compil` driver segfaults earlier that day). **What is inferred:** the four historical failures
   left no stderr (`run.sh` discarded it), so their attribution to VRAM exhaustion rests on the
   reproduction and on these overlaps, not on a captured message. The PRV16 olympus failure (21:04–21:45)
   has no kernel event in its window.
4. **Not xvfb / parallelism.** Two simultaneous exports under one Xvfb never failed (evidence 1). A
   different failure exists but is a setup error, not a flake: with no reachable X server the app panics
   `Failed to initialize gtk backend` (known pitfall; I also hit it when two `xvfb-run -a` started within
   a second and raced for a display number).
5. **Side finding (artificial load, not the flake).** With 12 busy loops at normal priority plus 2–4 exports at
   nice 10, every export stalled for > 7 min (rayon workers in `sched_yield`, the 4 earlier ones > 55 min), and
   continued to stall after the load ended. Not seen under any realistic load; recorded only as a
   limit — do not run preservation under a saturated CPU.

## Changes

* Harness `scripts/reference/preservation/run.sh` (notes repo): keeps stderr of a failed export and
  the GPU memory in `results/TAG.MODE.sha.err/`, retries a failed export once after 30 s, lists
  every retry in `TAG.MODE.sha.retried` and in the `RUN_DONE` line (`retried=N`), never hides it.
  Tested with a fake binary that fails once (326 ok, retried=1, error kept).
* Rule for preservation runs: no GPU test or other GPU job on the PC while `run.sh` runs
  (`nvidia-smi` must show ~0.5 GB used).

## Open (recommendation, not built)

A batch export could wait and retry a photo whose render hit GPU out-of-memory (the memory is usually
free seconds later). It touches the export planning (`used_paths` reserves a file name before
the render, so a retry inside the run would pick a `_1` name) and the queue's progress/`finished`
tracking, so it is a design decision for the coordinator; today the photo fails with a clear message and
the rest of the batch goes on. Files: `src-tauri/src/export_processing.rs` (`export_photo`, `plan`), `crates/gpu/src/gpu_processing.rs` (`gpu_scoped`).

Scripts: `sweep.sh` (stderr-keeping loop; needs `bc`-free python3), `vram_hog.cu` (`nvcc -o hog vram_hog.cu; ./hog 1800`).

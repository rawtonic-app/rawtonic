# AI models: inventory, licenses and hosting

Rawtonic's AI tools are optional: core editing needs no model and no ONNX
Runtime. This file records every model the app can download, where each file
comes from, its license and whether Rawtonic may redistribute it. It replaces
the open "model license/checksum inventory" of G65.

Status 2026-10-09. Every model comes from Rawtonic's own pack (see
Hosting); the inherited RapidRAW mirror (`huggingface.co/CyberTimon/RapidRAW-Models`,
no license or provenance information) is no longer used. The checks below
traced each file back to its authors' release by SHA-256 or by comparing the
weights tensor by tensor.

## Policy

- A model ships only when its authors grant a license for the weights that
  allows redistribution and commercial use (Apache-2.0, MIT, BSD, or GPL-3.0
  for AGPL compatibility). A tag on a third-party mirror is not enough.
- Training-data terms (research-only datasets such as Places365, ADE20K or
  SA-1B) are recorded as a residual risk; no model in the field is free of
  it, Adobe Firefly excepted.
- Rawtonic hosts the files it ships itself, built from the authors' files by
  `scripts/models/build_model_pack.py`, with the license texts beside them
  (`licenses/models/`). The app pins every file by SHA-256.

## Inventory

| File | Used by | Origin (verified) | License | Status |
|---|---|---|---|---|
| `sam_vit_b_01ec64_encoder.onnx`, `…_decoder.onnx` | Subject mask (point/box) | Inherited files: dynamically quantised exports of unknown origin. Their embeddings match Rawtonic's export of Meta's checkpoint (cosine 0.9995), so they derive from it. Replaced by Rawtonic's own export of Meta's `sam_vit_b_01ec64.pth` (`scripts/models/export_sam.py`). | Apache-2.0 (Meta) | Replaced by SAM 2.1 Hiera-T 2026-10-09 (the release assets stay for older builds) |
| `sam2.1_tiny_encoder.onnx`, `sam2.1_tiny_decoder.onnx` | Object/click and box masks, Quick Erase | Rawtonic's export of Meta's SAM 2.1 Hiera-T checkpoint (`sam2.1_hiera_tiny.pt` from dl.fbaipublicfiles.com/segment_anything_2/092824, SHA-256 7402e0d8…; README of facebookresearch/sam2 at 2b90b9f: "The SAM 2 model checkpoints … are licensed under Apache 2.0") by `scripts/models/export_sam2.py`; fp16 storage, reproducible (same SHA-256 in three exports). | Apache-2.0 (Meta) | Built 2026-10-09, in the pack |
| `birefnet_subject.onnx` | Subject and background masks | Rawtonic's export of BiRefNet's general checkpoint (Peng Zheng et al., `ZhengPeng7/BiRefNet`, revision e2bf8e4; model card `license: mit`, GitHub repository MIT) by `scripts/models/export_birefnet.py`; deformable convolutions as GridSample + matmul (ONNX Runtime has no DeformConv kernel), fp16 storage (470 MB; IoU ≥ 0.9998 to fp32 on 30 DUTS-TE images); reproducible (same SHA-256 in two exports). Trained on DIS5K (non-commercial research terms), DUTS, HRSOD, UHRSD, HRS10K, P3M-10k and others: residual risk under Training data. | MIT | In the pack since 2026-10-09 |
| `birefnet_fp16_coreml_*` (Core ML package: `Manifest.json`, `model.mlmodel`, `weight.bin`) | Subject and background masks on macOS (fast path) | The same BiRefNet checkpoint and revision converted from PyTorch with coremltools 9.0 by `scripts/models/convert_birefnet_coreml.py` (deformable convolutions decomposed as for ONNX; ML program, fp16, 464 MB; reproducible: same SHA-256 in every conversion, on any day). Masks score as the fp32 program's against subject references (Subject on Core ML in fp16 below). Without these files, or if Core ML fails, the app uses `birefnet_subject.onnx`. | MIT | Built 2026-10-11, in the pack |
| `birefnet_coreml_*` (Core ML package, fp32) | Subject and background masks on macOS, builds from 2026-10-10 to 2026-10-11 | As above in fp32 (927 MB; `--precision fp32` reproduces the pack's files). | MIT | In the pack; replaced by the fp16 program 2026-10-11 (the release assets stay for older builds) |
| `u2net.onnx` | Foreground mask (inherited) | Byte-identical to rembg's `u2net.onnx` (release v0.0.0), the export of Qin et al.'s U²-Net. | Apache-2.0 (weights), MIT (rembg) | Replaced by BiRefNet 2026-10-09 (the release asset stays for older builds) |
| `relight_geometry_da3_base.onnx` | Relight relative scene geometry and camera | DA3 **Base**, rights holder `depth-anything/DA3-BASE` revision `f4a6c9b3c95e41c82048423d3493a81ec3fa810e`; source `ByteDance-Seed/Depth-Anything-3` at `3d835ec1a5802d64a8b8b15f817a1ab54809bfe4`. Exported by `scripts/models/export_relight.py`. | Apache-2.0 | Self-host |
| `relight_people_normals_david_base.onnx` | Relight surface normals of people | Microsoft's DAViD Base normal model (`normal-model-vitb16_384.onnx`, github.com/microsoft/DAViD at `20a3eb6`, README "Model License": "DAViD models and runtime code are licensed under the MIT License"), unchanged; trained on Microsoft's synthetic SynthHuman (CDLA-2.0). | MIT | In the pack since 2026-10-15 |
| `depth_anything_v2_vits.onnx` | Depth masks | Byte-identical to fabio-sim/Depth-Anything-ONNX v2.0.0, the export of Depth Anything V2 **Small**. | Apache-2.0 (Small only; Base/Large/Giant are CC BY-NC 4.0) | Keep, self-host |
| ONNX Runtime 1.23.2 (macOS arm64 / x86_64) | Runtime for macOS AI tools | Unchanged Microsoft release libraries, SHA-256 pinned; fixes the shutdown crash reproduced on 1.22.0. | MIT + 1.23.2 third-party notices | Keep, self-host |
| ONNX Runtime 1.22.0 (six desktop libraries) | Legacy runtime (macOS before 2026-10-13; Windows and Linux before 2026-10-10) | Byte-identical to Microsoft's v1.22.0 release archives (all six platforms). | MIT + third-party notices | Keep, self-host |
| ONNX Runtime 1.30.0 (`libonnxruntime-1.30.0-linux-{x86_64,aarch64}.so`, `onnxruntime-1.30.0-windows-{x86_64,aarch64}.dll`) | Runtime on Windows and Linux (plugin providers need 1.24.4+) | Byte-identical to the libraries in Microsoft's v1.30.0 release archives. | MIT + third-party notices (`onnxruntime-1.30.0-ThirdPartyNotices.txt`) | Built 2026-10-10, in the pack |
| WebGPU plugin execution provider 0.4.0 (`…providers_webgpu-0.4.0-{linux,windows}-{x86_64,aarch64}`) | GPU for every model on Windows (Direct3D 12) and Linux (Vulkan), bundled beside the runtime | The library inside Microsoft's `onnxruntime-ep-webgpu` 0.4.0 wheels on PyPI (built from ONNX Runtime 1.30.0), unchanged; the Windows wheel's DirectX Shader Compiler DLLs are left out (Microsoft Software License Terms). | MIT; contains Dawn (BSD-3-Clause); notices as ONNX Runtime 1.30.0 | Built 2026-10-10, in the pack |
| CUDA plugin execution provider 0.2.0, CUDA 13 (`…providers_cuda-0.2.0-cuda13-{linux,windows}-x86_64`) | NVIDIA GPUs, optional (`install_models.sh --cuda`), with the user's own CUDA 13 and cuDNN 9 | Microsoft's GitHub release `plugin-ep-cuda/v0.2.0`, unchanged. | MIT (no NVIDIA library included) | Built 2026-10-10, in the pack |
| `siglip2_vision.onnx`, `siglip2_tags.bin` (+ `siglip2_text.onnx`, `siglip2_tokenizer.json` for own tags) | Auto tags | Rawtonic's export of Google's SigLIP 2 B/16 (`google/siglip2-base-patch16-224`, revision 75de2d5) by `scripts/models/export_siglip2.py`; reproducible (same SHA-256 in two exports). Replaced the inherited CLIP (OpenAI ViT-B/32, model card: deployed use "out of scope") on 2026-10-08. | Apache-2.0 | In the pack |
| `nlmf_coreml_*` (Core ML package), `nlmf.onnx` | AI Denoise (Detail → Noise reduction) | Nonlocal MatchFilter (Sánchez-Beeckman and Buades, MIT), Core ML and ONNX conversions by sheldonxxxx/RapidRAW-Nonlocal-Denoise at revision af6bf36, unchanged (hashes from its `distribution.json`) | MIT | In the pack |
| `nlmf_mixed640_macos13_coreml_*` (Core ML package) | AI Denoise version 3 on macOS | The same MatchFilter checkpoint (authors' release v1.0.0, SHA-256 pinned) converted by `scripts/models/convert_nlmf_coreml.py`: mixed precision (convolutions fp16, matching fp32), 640 px tiles, Core ML for macOS 13 like the app, through RapidRAW's vendored network and static packed sampler (sheldonxxxx/RapidRAW `denoise/`, commit 2375e17); reproducible. Version 3, the default for new RAW edits, runs it; version 1 and 2 recipes keep `nlmf_coreml_*`. A macOS 15 build of it (`nlmf_mixed640_coreml_*`, same weights, same results on macOS 26) went into pack-2026-10-18 first and is used by no build. | MIT | In the pack (from pack-2026-10-18) |
| `skyseg_upernet_convnext_t.onnx` | Sky mask | Rawtonic's export of UperNet + ConvNeXt-T on ADE20K (Meta's checkpoint from facebookresearch/ConvNeXt, MIT; transformers conversion `openmmlab/upernet-convnext-tiny`, revision 876ffc5) by `scripts/models/export_sky.py`; sky probability of ADE20K class 2, fp16 storage, reproducible (same SHA-256 in two exports). | MIT (Meta) | In the pack since 2026-10-09 |
| `skyseg_coreml_*` (Core ML package: `Manifest.json`, `model.mlmodel`, `weight.bin`) | Sky mask on macOS (fast path) | The same UperNet + ConvNeXt-T checkpoint and revision converted from PyTorch with coremltools 9.0 by `scripts/models/convert_sky_coreml.py` (the export's graph; ML program, fp32, 237 MB; reproducible with `PYTHONHASHSEED=0`: same SHA-256 in three conversions). Probabilities equal to PyTorch's (largest difference < 1e-4, mask IoU 1.0 on 13 photos), 0.06 s on the GPU of an M5 Pro against 0.35-0.8 s on the CPU. Downloaded on macOS for the first Sky mask (before 2026-10-19 only `install_models.sh` installed them); if Core ML fails, the app uses the ONNX model. | MIT | Built 2026-10-10, in the pack |
| `skyseg-u2net.onnx` | Sky mask (inherited) | Byte-identical to `JianyuanWang/skyseg` (a re-upload tagged MIT); originally from xiongzhu666's Sky-Segmentation repo (MIT code). The author states the model was trained on ADE20K plus private company data; the README does not license the ONNX file. | Unclear | Replaced 2026-10-09 |
| `nind_denoise_utnet_684.onnx` | AI Denoise (inherited tool) | NIND UtNet (Brummer, trougnouf/nind-denoise, GPL-3.0 code); the weights carry no license statement. | Unclear | Removed 2026-10-08: Rawtonic's NR measures better; the Denoise tool keeps BM3D and Apple RAW |
| `migan_pipeline_v2.onnx` | Object removal | MI-GAN 512 Places2 (Picsart AI Research), the ONNX pipeline its first author published (`andraniksargsyan/migan`, revision 406830d; linked from the official repository), unchanged. | MIT (weights, `LICENSE-WEIGHTS`) | In the pack since 2026-10-09 |
| `face_detection_yunet_2023mar.onnx` | People masks (faces) | YuNet 2023mar (Wu, Peng and Yu), OpenCV Zoo `models/face_detection_yunet` at commit f12e127 (Git LFS object), unchanged; `LICENSE` in that folder: MIT, © 2020 Shiqi Yu. Trained on WIDER FACE (residual risk under Training data). | MIT | In the pack since 2026-10-09 |
| `face_mesh_v2.onnx` | People masks (eyes, brows, lips, teeth) | MediaPipe Face Mesh V2 (Google), the `face_landmarks_detector.tflite` of `face_landmarker.task` (float16, version 1, storage.googleapis.com/mediapipe-models), converted by `scripts/models/export_faces.py` (tf2onnx, checked against the TFLite interpreter, reproducible: same SHA-256 in two exports). Model card: "Licensed under Apache License, Version 2.0". | Apache-2.0 (Google) | In the pack since 2026-10-09 |
| `selfie_multiclass_256.onnx` | People masks (hair, face and body skin, clothes, accessories) | MediaPipe multiclass selfie segmentation (Google; `selfie_multiclass_256x256.tflite`, float32, version 1), converted by `scripts/models/export_faces.py` (as above). Model card: "Licensed under Apache License, Version 2.0". | Apache-2.0 (Google) | In the pack since 2026-10-09 |
| `lama_fp16.onnx` | Inpainting (inherited) | LaMa (Samsung, Apache-2.0 code); no license statement for the big-lama weights; trained on Places365 (research/education terms). | Unclear | Replaced by MI-GAN 2026-10-09 |

Subject masks: BiRefNet against the inherited U²-Net (the app's
preprocessing: U²-Net letterboxed to 320 px and min-max scaled, BiRefNet
stretched to 1024 px as a probability) on 300 random DUTS-TE images,
threshold 0.5. Correction 2026-10-09: DUTS-TE was assumed held out from
both models' training, but the authors list it in the training set of
BiRefNet's general checkpoints (Training data below), so for BiRefNet these
are training images and its lead is overstated; U²-Net (DUTS-TR) did not
see them:

| Model | IoU mean / median | Share IoU > 0.9 | Boundary F (3 px) | MAE | CPU per image |
|---|---|---|---|---|---|
| U²-Net (inherited) | 0.715 / 0.836 | 33 % | 0.693 | 0.075 | 0.2 s |
| BiRefNet lite (Swin-T) | 0.802 / 0.914 | 55 % | 0.789 | 0.037 | 1.3 s |
| BiRefNet (Swin-L, shipped) | 0.883 / 0.937 | 71 % | 0.882 | 0.021 | 5.0 s |

On eight RAW samples (pumpkin, figurines, flower, toy, umbellifers, robin,
fruit bowl, geranium) BiRefNet's masks are complete and sharp where U²-Net's
were grey and incomplete (umbellifers, geranium pot, the figurines' gaps);
U²-Net also selected a calendar card beside the pumpkin. It runs on the CPU
(about 4-5 s once per photo, cached). ONNX Runtime's Core ML provider with a
persistent model cache (MLProgram, CPU and GPU, ONNX Runtime 1.22, M5 Pro,
2026-10-10, other load on the machine): first load 396 s, later loads
73-95 s each, a 6.4 GB cache for the 470 MB model, outputs equal to the
CPU's (cosine 1.000000); the encode itself was 4.4-58 s under GPU load from
other work (3.1 s measured on a quiet machine before) against 3.9-5.0 s on
the CPU. Not viable even in the background: every launch would spend over
a minute and 6.4 GB of disk for at most 1-2 s per photo. Lightroom's speed
needs a real Core ML conversion of the whole model (as for AI Denoise), not
the provider.

### Subject masks: Core ML and computing ahead (2026-10-10)

Two changes, on top of the ONNX model that stays the path on every
platform:

- **Computed ahead.** When the masking tools open (the Layers & masks tab
  or a selected layer; pointing at the layer panel's selection buttons or
  opening a component menu asks too), BiRefNet runs in the background for
  the open photo if its model is installed (nothing is downloaded). The
  mask is cached per photo and geometry (four photos); a click on Subject
  or Background during the run waits for it; a photo switch stops the run
  (ONNX Runtime's terminate flag). Not on every photo open: a run costs
  seconds of CPU.
- **Core ML on macOS.** `birefnet_coreml_*`, converted from the PyTorch
  checkpoint by `scripts/models/convert_birefnet_coreml.py` (deformable
  convolutions decomposed as for ONNX; coremltools 9.0, which needs torch
  2.7 and a fix for NumPy 2.5's scalar casts; protobuf maps written sorted
  and the manifest's random identifiers replaced, so the SHA-256 is the
  same in two conversions). Without its files, or when Core ML fails, the
  ONNX model is used.

Pre- and post-processing were also made parallel (the photo stretched to
1024², the mask scaled back to the photo): 45 MP 0.77 s and 1.53 s with the
image crate, 40 ms and 66 ms now, the same values
(`the_parallel_resize_equals_the_image_crate`).

Agreement with the ONNX model, held out: 47 public raw.pixls.us files of
recent cameras (rendered at 2048 px, not in BiRefNet's training data),
mask at 0.5, CPU and GPU (M5 Pro, coremltools):

| Core ML variant | IoU vs ONNX fp32: mean / min | Inference | Size |
|---|---|---|---|
| fp16 (shipped since 2026-10-11, see below) | 0.9972 / 0.979 (26 of 47 at 0.999 or more) | 0.20 s | 464 MB |
| fp16, normalisations, attention and divisions fp32 | 0.9971 / 0.973 | 0.26 s | 474 MB |
| int8 weights, fp32 compute | 0.980 / 0.642 | 0.56 s | 232 MB |
| fp32 (shipped 2026-10-10 to 2026-10-11) | 0.99999 / 0.9998 | 0.54 s | 927 MB |
| ONNX fp16 (the ONNX model), CPU | 0.99997 / 0.9994 | 5.6 s | 470 MB |

fp32 against the shipped ONNX fp16 model: 0.99997 / 0.9992 (the ONNX model
against its own fp32 export: 0.99997 / 0.9994). In the app's Rust path on
six RAWs (12-45 MP, `print_subject_core_ml_agreement`): IoU 0.99996 mean,
0.99981 min. fp16 shifted mask edges, hence fp32 at first; judged against
subject references instead of against fp32, fp16 turned out as good
(2026-10-11, below).

Compute units: CPU and GPU. With the Neural Engine (`All`) the first load
of the fp16 model compiled in ANECompilerService for about 15 minutes, and
runs were no faster (fp32: 0.56-0.77 s). Load times (CPU and GPU, fp32): compiling the
package 0.6 s; first load in the app 5.9 s (compile, SHA-256 of the files,
GPU set-up); later launches 1.8-3.8 s (most of it the SHA-256 of 927 MB);
the first inference after a launch 1.4-2 s, later ones 0.8-1.9 s in the
app under other GPU load (0.54 s in Python on a quieter GPU). The compiled
model (`birefnet_subject.mlmodelc`, 888 MB) stays beside the package; the
package's files are hard links to the downloaded ones.

Click on Subject → mask in the recipe, QA build (cc.rawtonic.qa, QA
channel, 50 ms polling), six RAW QA copies (Canon R5 45 MP, Nikon Z 7II
45 MP, Fujifilm X-T5 40 MP, Sony α7 IV 33 MP, Panasonic S5 24 MP, iPhone 12
Pro 12 MP). The machine ran other agents' builds and Photoshop throughout
(load 10-34, swap in use); times are of one run each:

| | Before (main) | After, ONNX | After, Core ML |
|---|---|---|---|
| Cold (no computing ahead; the photo open 6 s) | 8.2-26.3 s | 6.7-10.4 s (load 10); 4.6-17.8 s (load 18-27) | 1.5-2.0 s on 4 of 6; 4.5 s; first after a launch 5.5 s (incl. 3.8 s loading) |
| Warm (Layers tab open first) | – | 0.01-0.30 s | 0.08-0.16 s; first photo after a launch 3.3 s (its background run was still loading the model) |
| Background after Subject on the photo | 6.5-17.3 s | 0.04-0.09 s | 0.05-0.20 s |

On the first Core ML launch (compiling) the first photo took 15.2 s, the
others 1.3-2.9 s. Under GPU load from other processes single Core ML runs
took 8-15 s. A photo switch 2 s into a background run stopped it after
2.2 s; a click 1.5 s after the switch waited for the new photo's run
instead of starting a second one.

Lightroom Classic 15.6, same files (copies), fresh test catalog, Select
Subject about 2 s after switching photos, until its "Detecting subject"
message disappears (screen sampled every 0.2-0.3 s): iPhone 0.89 s, X-T5
0.70 s, R5 1.32 s, S5 1.57 s, α7 IV 2.15 s. Lightroom ran people detection
when the masking panel opened, but not Select Subject. So: warm, Rawtonic
answers in 0.1-0.3 s against Lightroom's 0.7-2.2 s; cold with Core ML
(1.5-2.0 s, first after a launch 5.5 s) about level with Lightroom on a
loaded machine; cold on the ONNX path (Windows, Linux, Macs without the
Core ML files) still 3-5× slower. Not measured: a quiet machine, mask
quality side by side with Lightroom, Windows/Linux (since 2026-10-10 the
WebGPU and CUDA providers, see GPU providers below; speed there is not
measured yet).

### Cold Subject and Sky, Sky edges (2026-10-10, second pass)

Starting point (Lightroom Classic 15.6 side-by-side, notes:
reference/lr-compare-2026-10-10.md): Subject clicked right after a photo
opened 0.62-0.68 s against Lightroom's 0.25-0.44 s; the first Subject after
a launch 3.1 s (1.85 s of it loading Core ML, about 1 s of that the SHA-256
of the 927 MB weights on every launch); Sky 0.9-1.5 s against 0.37-0.83 s;
the Sky edge on a desert horizon boundary F 0.53 against 0.81.

What changed:

- **Model files are verified once.** An installed model's SHA-256 is
  computed once; `<models>/verified-files.tsv` (written atomically) keeps
  the result with the file's size, modification time, status-change time
  and inode/device (creation time on Windows), and later launches trust a
  file that still looks the same. Writing to or replacing a file changes
  these (the status-change time cannot be set back by tools that keep
  modification times), so it is hashed again before use. Downloads are
  still checked byte for byte.
- **Sky loads on its own** (119 MB; before, the first Sky also loaded and
  hashed the depth model), and is **computed ahead and cached** like
  Subject: after Subject, for the open photo, per photo and geometry (four
  photos), cancelled on a photo switch (ONNX Runtime's terminate flag). A
  Sky click during a background run waits for it.
- **Ahead on opening a photo.** Once the masking tools were used in a
  session (Layers tab, the layer panel's selection buttons or component
  menus, or a Subject/Sky mask), every photo that stays open 0.5 s in
  Develop gets Subject, then Sky, computed in the background (about 0.6 s
  of GPU and 0.4 s of CPU per photo on an M5 Pro); quick stepping skips it,
  a switch cancels it.
- **Loaded ahead after a launch** (replaced the same day, see "When the
  masking models stay in memory" below). When the masking tools were used before
  (`rawtonic.masks.used` in the webview's storage), the Subject and Sky
  models load 3 s after the workspace appears (installed models only;
  nothing is downloaded) and run once on a small synthetic picture, so the
  first click after a launch waits neither for the load nor for the slower
  first inference. Cost (M5 Pro, idle 25 s after launch, both Core ML
  models): footprint 1385 MB against 114 MB without (peak 5.6 GB during the
  load against 0.6 GB), 2.4 s of loading and one inference of each model
  per launch; the same memory a first Subject click would take anyway.
- **Sky on Core ML (macOS)** when `skyseg_coreml_*` is installed
  (`convert_sky_coreml.py`, see the inventory): 0.06-0.1 s instead of
  0.35-0.8 s on the CPU, the same masks on the 13 photos below. Not yet in
  the pack, so the app does not fetch it on demand; the ONNX model stays
  the path everywhere else.
- **Faster Sky pre- and post-processing**: the photo fitted into 512² and
  the 2048 px guide resized in parallel (the image crate's resize is
  single-threaded), the mask scaled back in parallel, and the guided
  filter's own resizes too (also used for SAM's edges); the same values
  (`the_parallel_resize_equals_the_image_crate`, now also for 8-bit
  photos, float planes and the aspect-keeping fit).
- **Sky edges decided by colour** (`split_sky_by_colour`). The network
  predicts at a quarter of 512 px; the lightness-guided filter (radius 1/50,
  as before) left bright rock or snow on a mountain ridge half sky and blurred
  thin branches. Now, where the filtered mask is neither fully sky nor
  fully ground, a pixel counts as sky as far as its colour (lightness and
  the two colour differences, equal weight) lies toward the sky's colour
  nearby rather than the ground's, both averaged from where the network is
  sure (≥ 0.95 / ≤ 0.05) in a window of 1/10 of the photo (blended with a
  three times wider one where few sure pixels are near), at twice the
  linear slope around halfway. Where sky and ground nearby have similar
  colours, or one of them is absent, the filtered mask stays.

Sky edge evaluation: 13 CC0 raw.pixls.us photos (SHA-256 checked against
raw.pixls.us' list; rendered with rawpy at 3000 px: desert and mountain
horizons, red rock against clouds, a church, a water tower, a spire and a
tree in silhouette against sunset sky, bare branches, a lone tree, a
road), reference masks made per photo with colour rules or GrabCut and
checked visually (not with either app's masks); IoU and boundary F (3 px)
at 1024 px, mask at 0.5; "leak" = mean mask value on ground within 24 px of
the reference edge, "miss" = mean missing sky there
(`print_sky_masks` with `SKY_EDGE`):

| Edge refinement | IoU | Boundary F | Leak | Miss |
|---|---|---|---|---|
| Lightness guided filter (before) | 0.960 | 0.587 | 0.203 | 0.109 |
| Colour guide (3-channel guided filter) instead | 0.954 | 0.529 | – | – |
| Radius 1/200 instead of 1/50 | 0.973 | 0.674 | – | – |
| **Lightness filter + colour decision (shipped)** | **0.982** | **0.814** | **0.067** | **0.038** |

Desert horizon (the Lightroom comparison's weak case) boundary F 0.389 →
0.90, desert hills 0.42 → 0.93, lone tree 0.64 → 0.82; IoU better on all
13, boundary F on 11 (church 0.817 → 0.793, spire 0.907 → 0.897). The
references were partly
made with colour rules, which may favour a colour decision; the GrabCut
references (church, hills, mountain, road, sedona) and the lightness ones
(silhouette, spire) improved as well.

Click → mask in the recipe, QA builds (cc.rawtonic.qa, `NSAppSleepDisabled`)
of main (Before) and this change (After), the same session of the 23
photos, synthetic DOM clicks through the QA channel (no focus taken),
recipe polled every 10 ms (the poll that saw the mask counts), the sky
Core ML files installed in the After build, 2 launches per build plus
first-click launches; machine load (1 min) with each range. "Cold": the
Layers tab open, next photo, click as soon as it is ready; "open 1 s":
Layers closed (masking used earlier in the launch), click 1 s after the
photo is ready; "warm": Layers open, click 6 s after; "first": the first
photo of a launch, clicked as soon as it is ready, about 30 s after launch
(the harness waits for the library).

| | Before | After | Lightroom 15.6 (lr-compare, other method) |
|---|---|---|---|
| Subject, cold | 0.60-0.70 s (load 4-8) | 0.62-0.70 s (load 4-8) | 0.25-0.44 s (load 4-10) |
| Subject, open 1 s | 0.64-0.73 s (load 5-6) | 0.08-0.13 s (load 4-9) | – |
| Subject, warm | 0.02-0.03 s | 0.02-0.05 s | no precompute |
| Subject, first after launch | 1.50-2.02 s (load 4-10) | 0.64-0.65 s (load 7-9); 2.7 s on the first launch after the update (hashes once, Core ML compiles the sky package) | – |
| Sky, cold | 1.00-1.44 s (load 5-6) | 0.18-0.20 s (load 3-4); 0.39-1.21 s (load 10-13) | 0.37-0.83 s (load 14-20) |
| Sky, open 1 s | 1.22-3.25 s (load 5-14) | 0.17-0.20 s (load 3-12) | – |
| Sky, warm | 1.03-1.86 s | 0.02-0.03 s | – |
| Sky, first after launch | 2.78-3.45 s (load 8-11) | 0.18 s (load 7) | – |

Inside a Sky run (After, app log): input 2-40 ms, Core ML 60-100 ms (ONNX
CPU 650-790 ms before), edges median 73 ms (39-113 ms; 0.2-1.1 s while other
processes took the cores), PNG 0-3 ms. Subject's cold time is BiRefNet on
Core ML itself (fp32, 0.6 s on this GPU): pre- and post-processing take
under 30 ms, so a click within 0.6 s of opening a photo stays at about
0.6 s. The fp16 conversion (0.20 s, edges moved: IoU 0.997 / 0.979 against
fp32) was not revisited; Core ML's low-precision GPU accumulation for the
fp32 model changed nothing (0.62-1.3 s, IoU 0.99999). On-screen timing
(in-app window capture after the click) was tried and left out: the new
layer's own redraw changed the picture before the mask arrived, so the
first change did not mark the mask.

Verification: ignored tests `inpainting_fills_only_the_hole` (`RAWTONIC_INPAINT_MODEL`) and `sky_mask_marks_the_sky` (`RAWTONIC_SKY_MODEL`, `RAWTONIC_SKY_PHOTO`) in `src-tauri/src/ai_processing.rs`, with `ORT_DYLIB_PATH`; Subject on Core ML against ONNX: `print_subject_core_ml_agreement` (`AI_FILES`, `AI_MODELS`, `SUBJECT_COREML`, optional `SUBJECT_UNITS`), and `convert_birefnet_coreml.py OUT --check IMAGES` against PyTorch; two Core ML programs' app masks for reference comparisons: `print_subject_precision_masks` (`AI_FILES`, `AI_OUT`, `SUBJECT_COREML_A`, `SUBJECT_COREML_B`).

### When the masking models stay in memory (2026-10-10, third pass)

Loading Subject and Sky 3 s after every launch (above) held about 1.3 GB
all day. User decision 2026-10-10, implemented in `src-tauri/src/ai_lifecycle.rs`,
`memory_pressure.rs` and `useAiMasking.ts`:

- **Loaded with Develop.** For someone who used the masking tools before,
  entering Develop starts loading Subject and Sky at once (beside the
  photo's decoding, `preload_ai_mask_models`, load only), and the first
  photo that stays open 0.5 s gets Subject, then Sky, then SAM 2.1's
  encoding computed ahead: the photo's own masks are the warm-up (a
  synthetic warm-up cost the same GPU time and another ~4.4 GB peak).
  After recent masking use (4 minutes) every photo gets them ahead, as
  before. Nobody else loads anything ahead: SAM's background encoding,
  which main ran on every photo for everyone with the model installed, now
  follows the same rule (after Subject and Sky, so their working memory
  does not add up).
- **Dropped again** (models, SAM's embedding, the depth map, the faces
  found; MI-GAN and the face models too): after 5 minutes without masking
  use when nothing runs; when the Library is shown; at once on memory
  pressure (macOS: the kernel's memory-pressure source; Windows and Linux,
  or if that fails: available physical memory below a tenth, at most
  1 GiB, read every 5 s), then also the tagging models and the finished
  Subject/Sky masks, and background runs are cancelled. A run that holds a
  model finishes with it; a load that finishes after an unload is used once
  and not kept. After an idle or pressure unload, opening photos loads
  nothing until the masking tools are used again (no reload churn). AI
  Denoise is not managed (see open points).

**Where the 5.6 GB "loading peak" came from** (`print_mask_model_memory`,
M5 Pro, Core ML fp32, CPU and GPU): not the load. Loading Subject takes
0.96 GB resident (its weights on the GPU), Sky 0.23-0.32 GB; every
**Subject inference** then holds about 4.4 GB more (Core ML's GPU
intermediates of BiRefNet at 1024², fp32) for 1-2 s after it returns, and
this recurs on every run, warm-up or click, not only after a load. Sky
adds about 0.1 GB. Core ML's optimization hints (reshape frequency,
specialization) and low-precision accumulation changed nothing; Core ML on
the CPU only peaks at 2.1 GB but takes 2.5 s per run; fp16 (half the
intermediates) moved mask edges (above; judged against references it was
as good and replaced fp32 on 2026-10-11, about 1.6 GB per run, see "Subject
on Core ML in fp16"). Unloading both returns to
0.3-0.6 GB above the start (Core ML's framework state; a reload then
takes 0.3 s instead of 0.8 s). What was removed: the synthetic warm-up run,
i.e. one 4.4 GB peak per launch for people who do not mask in that
session, and SAM's encoding running at the same time. What remains: the
4.4 GB transient of each Subject run (open).

ONNX Runtime (macOS 1.22, CPU): after a session is dropped its memory is
not given back to the system (SAM 2.1 encoder 1.6-1.7 GB, sky 1.0 GB stay
as live large allocations; disabling the CPU arena does not change it), but
it is reused by the next load: four load/run/drop cycles stayed at
1.57-1.73 GB (SAM) and 1.02 GB (sky). So unloading an ONNX model does not
lower the footprint on macOS; not measured on Windows/Linux.

App measurements: QA builds of main (#70, 0fcdb56) and this change, the
same session of ten CC0 raw.pixls.us RAWs (12-61 MP), QA channel (no focus
taken), footprint of the app and its WebKit helpers (`footprint`), Core ML
Subject and Sky installed. Machine load (1 min) 2-7 throughout.

| Footprint (MB) | main | This change |
|---|---|---|
| Library, 25 s after launch (masking used before) | 1669 | 310-330 |
| Develop, one photo, masking never used | 3123 | 1155 |
| Develop, one photo, models loaded, 20 s after opening | 4435 | 4415-4435 |
| Library, 10 s after leaving Develop | 4444 | 2029-2036 |
| Develop again, then memory pressure (QA `ai.memory_pressure`) | 4429 (no handling) | 4271-4289 → 2078-2079 |
| Develop left open: 4 min / 5 min after opening | – | 4404 / 1995-1998 (idle unload) |
| Ten RAWs, masking never used (budget 2 GB) | 4353 | **1631** |
| Ten RAWs, masking used on the first photo | 5769 | 5749-5761 (over the budget) |
| App's peak: Develop entry with models / ten RAWs with masking | 5968 (warm-up at launch) / 11655 | 10047-10419 / 11411-11781 |

Click → mask in the recipe (synthetic DOM click, recipe polled every 10 ms;
same session; seconds; load 2.7-7.8):

| | main | This change | Lightroom 15.6 (lr-compare) |
|---|---|---|---|
| Subject, first photo after launch, clicked as soon as it is ready | 0.89-0.90 | 1.21-1.31 (models loading, first inference) | first after launch ~2.1 |
| Subject, first photo, clicked 1 s after it is ready (used before) | 0.73 | 0.43-0.46 | – |
| Subject, next photos 1 s after opening (masking used) | 0.03-0.49 | 0.03-0.44 | 0.25-0.44 (cold) |
| Subject, Library and back, a new photo clicked as soon as it is ready | 1.15 (no unload) | 1.78-2.44 (reload) | – |
| … and the next photo 1 s after | 0.60 | 0.50-0.65 | – |
| Sky, first photo as soon as ready | 0.42-0.46 | 0.47-0.56 | – |
| Sky, 1 s after opening | 0.25-0.32; 0.30 first photo | 0.22-0.30; 0.31-0.32 first photo | 0.37-0.83 |
| Sky, after Library and back, as soon as ready | 0.79 | 0.81-0.83 | – |

Open: the 4.4 GB transient of each Subject run on Core ML (fp16 or a
split model would halve it; edge quality first — fp16 since 2026-10-11,
about 1.6 GB); ten RAWs with masking in
use stay at ~5.7 GB, far over the 2 GB budget; the first Subject click
right after opening or after a Library round trip is 0.3-1.3 s slower than
main (it loads, and the first inference after a load is slower); ONNX
Runtime keeps its memory after unloading on macOS; AI Denoise's model stays
loaded once a photo used it (not changed); the kernel's memory-pressure
notification itself was not raised (needs root; the handler was run through
the QA channel), the polling fallback is untested on Windows/Linux.

Verification: `cargo test -p rawtonic-app ai_lifecycle memory_pressure`
(idle timer, busy runs, unload during a run, loads after an unload,
what each reason drops, low-memory hysteresis, the macOS source),
`src/hooks/useAiMasking.test.ts` (load on entering Develop and on the first
photo, never for never-users, recent use, Library unload, idle and
pressure events, SAM after Subject and Sky, use reporting), and the
ignored `print_mask_model_memory` (`SUBJECT_COREML`, `SKY_COREML`).

### Subject on Core ML in fp16 (2026-10-11)

fp16 had been set aside because its masks differ from fp32's. Judged
against subject references instead (user decision 2026-10-11: switch if
the quality is equal), it is as good, two to three times faster and holds
a third of the memory, so the app now uses `birefnet_fp16_coreml_*`
(package `birefnet_fp16.mlpackage`); the fp32 files stay in the pack for
older builds. Windows, Linux and Macs without the Core ML files keep the
ONNX model (fp16 storage, fp32 compute; unchanged).

**Quality.** 36 public raw.pixls.us RAWs (CC0, 3-75 MP: 7 people with
hair, 13 furry animals and plush toys, 6 birds, 5 plants and insects, 5
objects), decoded as the app does; the app's mask (`run_subject_model`,
mask at the photo's size; Subject has no edge refinement) from both
programs (`print_subject_precision_masks`). References: SAM 2.1 Large
(not used by the app), box and clicks, two passes, each checked on an
overlay; a second reference set from SAM 2.1 Base+ with the same prompts
measures how uncertain the references themselves are. Criterion, fixed
before the comparison: fp16 is equal if its mean change in IoU and in
boundary F (3 px, at 1024 px) is within that uncertainty, its 95 %
bootstrap interval stays above minus the uncertainty, no photo loses more
than its own uncertainty (at least 0.005 IoU / 0.01 BF), and 100 % crops of
the five worst photos show no worse edge.

| | fp32 | fp16 | mean change (95 % CI) | references' uncertainty |
|---|---|---|---|---|
| IoU | 0.9170 | 0.9169 | −0.0001 (−0.00014 … −0.00005) | 0.0105 |
| Boundary F, 3 px | 0.6912 | 0.6899 | −0.0013 (−0.0023 … −0.0005) | 0.050 |

Largest losses on one photo: IoU −0.0004, BF −0.012 (a long-haired cat,
whose own uncertainty is 0.034). fp16 against fp32 at the photo's size:
IoU 0.9993 mean, 0.9979 min; the soft masks differ by 0.011 on average
along edges, in 1-3 px bands; on 100 % crops the two cannot be told apart.
Equal by the criterion (the small systematic loss is a hundredth to a
fortieth of the references' uncertainty).

**Speed and memory** (M5 Pro, CPU and GPU):

| | fp32 | fp16 |
|---|---|---|
| Network alone, loaded (synthetic input, `print_mask_model_memory`) | 0.51 s; first run after a load 0.72-0.74 s | 0.17 s; 0.39-0.40 s |
| `run_subject_model` on 36 RAWs (input and full-size mask included) | 0.53-0.54 s | 0.19-0.21 s |
| Load (compiled model, Core ML cache warm) | 0.29-0.31 s | 0.24-0.26 s |
| Resident after loading Subject (first load in a process) | 0.96-0.98 GB | 0.49-0.51 GB |
| More while loaded once it has run | about 4.4 GB | about 1.6 GB |
| Size: pack files / compiled model / Core ML's own cache (`~/Library/Caches/<app>/com.apple.e5rt.e5bundlecache`) | 927 MB / 888 MB / 1.1 GB | 464 MB / 446 MB / 0.67 GB |

In the app (QA builds of this change and of main, identifiers
`cc.rawtonic.qa.fp16` / `.fp32`, App Nap off, the same session copy of ten
CC0 RAWs, the webview's storage removed before every launch so nothing was
computed ahead for the first photo; X-T5 40 MP cat clicked 5 s after it
opened; app-process footprint sampled every 10 ms):

| | fp32 | fp16 |
|---|---|---|
| Cold click → mask (app log), 5 launches | 1.42-1.62 s (load 0.32-0.56 + first run 0.99-1.07) | 1.26-1.31 s (load 0.73-0.76 + first run 0.49-0.50) |
| Cold, screen recording at 60 fps, pointer on Subject → overlay (app in front; layers-panel method) | 2.45-2.52 s (3 launches) | 1.15-1.48 s (4 launches) |
| A photo's run with the model loaded (computed ahead, 24 MP) | 0.80-0.89 s | 0.27-0.29 s |
| App footprint: before → highest during the cold run | 1.6-1.7 → 7.3-7.6 GB | 1.6-1.7 → 4.0-4.3 GB |
| Highest of a launch (cold photo, two more computed ahead) | 8.5-8.9 GB | 5.2-5.5 GB |
| Ten RAWs with masking (each computed ahead), 20 s after the last | 3.15-3.17 GB (peak 8.8 GB) | 2.73-2.75 GB (peak 5.4-5.5 GB) |
| First run for a new app identity (Core ML builds its cache) | 11.7 s | 14.5 s |
| Core ML cache deleted before the launch | 4.7-5.0 s | 3.6-3.8 s |

The footprints are the app process only (WebKit's helpers not included,
unlike the #71 table above), on a different set of ten photos. Ten RAWs
with masking stay above the 2 GB budget with fp16 too. The cold click is
now mostly the model load (0.7 s in the app; why fp16 loads slower in the
app than fp32 and than in the tool is not known); loading and warming the
model before the click is the next step.

## GPU providers on Windows and Linux (2026-10-10)

Every model also runs on a GPU on Windows and Linux: ONNX Runtime 1.30.0
with Microsoft's WebGPU plugin (Direct3D 12 or Vulkan, any vendor), or
NVIDIA's CUDA plugin when installed; the CPU stays the fallback, and a model
is used on a GPU only after its output there matched the CPU's once
(`crates/onnx`). Choice, licences and measurements: ADR 0011
(`docs/decisions/0011-gpu-ai.md`). On Metal (the same plugin on macOS, as a
stand-in GPU) outputs matched the CPU's (mask IoU 1.0) and BiRefNet ran in
1.3-2.5 s against 3.2-4.7 s on the CPU of a loaded M5 Pro; on Windows and
Linux GPUs neither speed nor correctness is measured yet. Benchmark:
`rawtonic ai-bench --provider webgpu --image photo.jpg`.

## Training data (residual risk)

The policy accepts research-only training data as a residual risk rather
than a reason to exclude a model whose weights are licensed. For the two
segmentation models with the largest training sets, concretely:

- **BiRefNet** (general checkpoint, Swin-L, the shipped `birefnet_subject.onnx`;
  the Swin-T "lite" variant has the same list). The authors' model zoo
  (github.com/ZhengPeng7/BiRefNet README, "general use") lists DIS5K-TR,
  DIS-TEs, DUTS-TR and DUTS-TE, HRSOD-TR/TE, UHRSD-TR/TE, HRS10K-TR/TE,
  P3M-10k (TR-P3M-10k, TE-P3M-500-NP, TE-P3M-500-P) and
  `schirrmacher/humans`. Terms as published: DIS5K "available for
  non-commercial use in research or educational purpose", redistribution
  of the data prohibited (DIS5K Terms of Use); P3M-10k under the MIT
  license by its release agreement (the images "from websites with free use
  licenses", copyright with their owners; P3M-500-NP are celebrity photos
  without face blurring); `schirrmacher/humans` tagged Apache-2.0 on its
  dataset card; DUTS (images from ImageNet DET and SUN), HRSOD, UHRSD
  (Flickr/Pixabay images "with free copyright") and HRS10K: no license
  statement in their READMEs. So at least DIS5K is explicitly
  non-commercial; the weights themselves are MIT. Other projects declined
  BiRefNet for this reason; Rawtonic keeps it under the policy above (no
  data is redistributed, the weights' license is clear). Because DUTS-TE is
  part of this checkpoint's training set, the DUTS-TE comparison with
  U²-Net under Subject masks is **not** held out for BiRefNet (it is for
  U²-Net, trained on DUTS-TR); it overstates BiRefNet's lead by an unknown
  amount. The RAW-sample comparison beside it is unaffected.
- **SAM 2.1** (`sam2.1_tiny_*.onnx`). SAM 2 paper (arXiv 2408.00714v2,
  whose results are the SAM 2.1 checkpoints): image encoder initialised
  from MAE-pretrained Hiera (ImageNet-1K), pre-trained on SA-1B, then
  trained on SA-V (manual masklets), Meta's internal licensed videos, a
  subset of SA-1B and the open video datasets DAVIS, MOSE and YouTube-VOS.
  Terms: SA-1B under Meta's SA-1B Dataset Research License (computer-vision
  research use; images licensed from a photo company); SA-V CC BY 4.0;
  MOSE CC BY-NC-SA 4.0 ("non-commercial research purpose only"); ImageNet
  non-commercial research terms; the internal videos are not public. The
  checkpoints themselves are Apache-2.0. SAM ViT-B (the model it replaces)
  was trained on SA-1B, so the residual risk is the same in kind.

- **Face models** (2026-10-09). YuNet was trained on WIDER FACE (images of
  the WIDER dataset from the web; its Hugging Face dataset card, CUHK-CSE/wider_face,
  gives CC BY-NC-ND 4.0; libfacedetection.train);
  the two MediaPipe models on Google's own data (Face Mesh V2's model card:
  images from smartphone cameras captured through an AR application; the
  segmentation model's card names no public dataset). The weights' licenses
  (MIT, Apache-2.0) are the rights holders'; WIDER FACE is a residual risk of
  the kind above.

## People masks: faces, landmarks and parts

Lightroom's People mask selects parts of everyone or of one person: entire
person, face skin, body skin, eyebrows, eye sclera, iris and pupil, lips,
teeth, hair, clothes. Rawtonic builds it from three small models
(`crates/faces`, 21 MB together):

1. **Faces**: YuNet on the photo fitted into 640 × 640, in two passes (the
   whole 640 and 45 % of it, since YuNet was trained on faces of about 10 to
   300 px; a face filling a portrait is found in the second pass), score
   ≥ 0.6, non-maximum suppression.
2. **Landmarks**: Face Mesh V2 on a 256 × 256 crop 1.5 × the face, rotated
   so the eyes are level (MediaPipe's own preprocessing); when that finds no
   face, crops centred on the eyes, nose and mouth (1.5 and 1.9 × the face;
   7 more of 235 faces get landmarks, faces looking down mostly not), then
   crops turned by ±25° and ±45° against the keypoints' roll (tilted heads,
   faces looking down far: 12 more of 231 holiday faces), then once more on
   the crop around the first landmarks. A face counts as seen when the
   presence output is ≥ 0.5 (≥ 0.85 for turned crops: one fitted a face to
   the back of a child's head at 0.79) and the parse sees face skin under
   the nose or on a cheek; profile views mostly fail and keep no landmarks
   (no eye, brow, lip or teeth parts; skin and hair still work, and the face
   retouch builds under-eye areas and the neck from the keypoints where they
   describe an upright face).
3. **Parse**: the multiclass segmentation once on the whole photo and once
   per face on a crop 3.3 × the face, where the face fills the frame as in
   the selfies it was trained on. A pixel's class probability is the
   crop-weighted mean of the crops covering it (the whole-photo parse counts
   5 %), at a working size of at most 2048 px, fitted to the photo's edges
   by a guided filter, then scaled up.
4. **Parts**: skin, hair, clothes and the entire person from the parse;
   eyes, brows, lips and teeth from the landmark outlines at full
   resolution, cut out of the face skin (eyes with a lash margin). Brows are
   the brow outline where the photo is darker than the skin between the
   brows; teeth the inner mouth where it is light and unsaturated; the iris
   the circle through the iris landmarks within the eye outline. Feature
   parts count only where the parse sees the face (not sunglasses, a hand or
   food in front of it). One person: head parts from that face's crop only,
   body parts by a soft nearest-face assignment stretched downwards.

Measured on seven holiday photos (12-13 MP JPEG previews; portraits with and
without sunglasses, two children, a group at a table, three people small in
a landscape), M5 Pro, ONNX Runtime 1.22 on the CPU, other load on the
machine: analysis 0.13-0.77 s per photo (once; the app keeps it), one part
mask 0.2-0.3 s. Every face was found, including a profile and faces turned
90° in previews without orientation, and two tight head-and-shoulders crops
(one face filling the frame). Visual check of the overlays: face skin stops
at the hairline, eyes and lips follow their outlines, teeth are separated
from the lips, sunglasses are left out of the eyes and the skin, the neck is
body skin, hands are body skin. Weak: a bread roll in a child's mouth was
taken as lips; for one person, body parts are split by distance between
faces, not by instance (people hugging can swap a shoulder).

On all 304 photos of the same holiday shoot (embedded JPEG previews, some
without orientation): faces on 154 photos, 235 faces, every one of them a
real face on a contact-sheet check (a pink balloon that YuNet's smaller pass
took for a face was dropped by requiring landmarks or 30 % face skin in the
box); mean 0.33 s per photo. 137 of the 235 faces got landmarks; the other
98 are faces looking down or turned away, where Face Mesh's presence stays
low (a threshold of 0.1 instead of 0.5 adds only 9), so for them the People
mask offers skin, hair and the entire person but no eyes, brows, lips or
teeth. The app's own run on a 51 MP RAF stored a face-skin mask with the
eyes, lips and sunglasses left out; the guided filter's faint echoes of the
photo's edges (a shirt's outline, stubble) are cut by levels at 0.1/0.9.

Not measured: Lightroom's People masks (they cannot be scripted), which need
a side-by-side use test on the same photos; Capture One has no People mask.

## Click and box masks: SAM 2.1 against SAM ViT-B

Meta's SAM 2.1 checkpoints (Apache-2.0 by the README of
facebookresearch/sam2, which names "the SAM 2 model checkpoints"), exported
by `scripts/models/export_sam2.py` (encoder and decoder with point/box
prompts, fp16 storage, the same SHA-256 in three exports), against the
shipped SAM ViT-B export, on 500 random DUTS-TE images (seed 0) with
ground-truth masks. DUTS-TE is not in SAM's or SAM 2's stated training data
(see Training data). Prompts from the ground truth: a click at its
innermost pixel, a box around it; 3 clicks = SAM's interactive protocol
(further clicks at the innermost pixel of the larger error, the previous
logits as mask input). "App" = `run_sam_decoder` as the app runs it (the
prompt, then a second pass with interior/exterior points, the first mask's
box and the mask as input), replicated in Python (`scripts/models/eval_sam_prompts.py`); decoder mask before the
guided-filter refinement. Mean IoU / share of masks with IoU > 0.9 /
boundary F (3 px):

| Model | App click | App box | 1 click | 3 clicks | Box |
|---|---|---|---|---|---|
| SAM ViT-B (shipped) | 0.374 / 5 % / 0.31 | 0.563 / 10 % / 0.44 | 0.458 / 11 % / 0.42 | 0.844 / 49 % / 0.80 | 0.811 / 42 % / 0.78 |
| SAM ViT-B, app flow fixed ¹ | 0.469 / 13 % / 0.44 | 0.818 / 46 % / 0.80 | 0.525 / 18 % / 0.49 ² | – | – |
| **SAM 2.1 Hiera-T** | **0.654 / 37 % / 0.65** | **0.870 / 61 % / 0.87** | 0.647 / 34 % / 0.64 | 0.896 / 72 % / 0.88 | 0.864 / 57 % / 0.85 |
| SAM 2.1 Hiera-S | 0.654 / 36 % / 0.65 | 0.870 / 64 % / 0.87 | 0.646 / 34 % / 0.64 | 0.894 / 72 % / 0.89 | 0.863 / 62 % / 0.85 |
| SAM 2.1 Hiera-B+ | 0.676 / 39 % / 0.68 | 0.869 / 63 % / 0.88 | 0.670 / 38 % / 0.67 | 0.897 / 73 % / 0.89 | 0.866 / 62 % / 0.87 |

¹ The app's second pass is wrong for SAM ViT-B: it stretches the first mask
to 256 × 256, but ViT-B's low-res frame is the padded square (the photo in
its top-left part), so on every non-square photo the mask input is
misplaced and the second pass destroys a good box mask (0.811 → 0.563);
the single click also lacked SAM's padding point. Fixing both helps, but
SAM 2.1 stays well ahead. SAM 2's frame is the stretched square, so the
existing second pass is right for it. ² Best of ViT-B's three multimask
candidates by predicted IoU (SAM's recommended single-click use).

Speed and size (M5 Pro, ONNX Runtime 1.22.0, median of five runs, other
load on the machine: load average about 5):

| Model | Files (enc + dec) | Encode, CPU | Encode, Core ML EP | Decode one pass, 24 MP / 6 MP |
|---|---|---|---|---|
| SAM ViT-B (shipped, uint8) | 100.7 + 4.8 MB | 1.80 s | 3.0 s | 36 / 19 ms |
| SAM 2.1 Hiera-T (fp16) | 55.1 + 8.5 MB | **0.58 s** | 0.15 s (28 s first compile) | 33 / 15 ms |
| SAM 2.1 Hiera-S (fp16) | 69.4 + 8.5 MB | 0.63 s | 0.17 s (36 s) | 32 / 15 ms |
| SAM 2.1 Hiera-B+ (fp16) | 139.4 + 8.5 MB | 1.05 s | 0.32 s (80 s) | 32 / 15 ms |

The app runs SAM on the CPU (no Core ML provider). A persistent Core ML
model cache (ONNX Runtime 1.22's `ModelCacheDirectory`, MLProgram, the
`ort` crate's `with_model_cache_dir`) was measured on 2026-10-10 (M5 Pro,
one process per load as in an app launch, a fresh cache first):

| Hiera-T encoder | Session load | Encode |
|---|---|---|
| CPU | 0.46-0.64 s | 0.58-0.59 s |
| Core ML, no cache | 32 s | 0.24 s |
| Core ML, cache: first load | 27-29 s | 0.15 s |
| Core ML, cache: later loads | **5.4-6.2 s** | 0.15 s |

Core ML takes 444 of 515 nodes in 21 partitions (MatMuls with a dimension
above 16384 stay on the CPU). The cache keeps the compiled models, but every
later load still spends about 5 s in Core ML, and the cache is 466 MB for a
55 MB model. Outputs equal the CPU's (cosine 1.000000, largest relative
difference < 1e-4), so masks would be unchanged. Neural Engine only
(`CPUAndNeuralEngine`) encodes slower (1.4-2.7 s); static input shapes and
`FastPrediction` do not shorten the load (7-15 s with other load). Not
enabled: a launch would cost 5 s (28 s the first time) and 466 MB to save
0.43 s per photo, so the first click after a launch gets slower and the
saving pays only after about a dozen new photos. If it is revisited: load in the background beside the CPU
session, key the cache directory by the model's SHA-256 (ONNX Runtime's own
key is derived from the model path and graph, not the weights).
fp16 storage against the
fp32 export (100 images): mean IoU equal to ±0.0005 in every mode.

Decision (2026-10-09): **SAM 2.1 Hiera-T** replaces SAM ViT-B for the
Object/click masks and Quick Erase: +0.28 IoU for a click and +0.31 for a
box in the app's flow (+0.19 / +0.05 against a fixed ViT-B flow), three
times faster to encode and 40 % smaller. Hiera-B+ is 0.02 better for a
click only, at 2.3 × the size and 1.8 × the encode time. Masks already in
recipes are stored as PNG (`maskDataBase64`) and render unchanged; only new
clicks and boxes use SAM 2.1. Not measured: Lightroom's and Capture One's
object selection (not scriptable); that needs a side-by-side use test.

### Edge refinement

The app fits the decoder's hard mask to the photo with a feather and a
guided filter (`refine_sam_mask`). Its radius was 0.75 % of the long edge,
clamped to 8-24 px: on the 300-400 px DUTS-TE photos 8 px, about five cells
of the decoder's 256 × 256 low-res mask, which smeared the edge and made the
mask worse than the unrefined one. Since 2026-10-10 the radius is one such
cell (long edge / 256, at least 1 px; 23 px for a 6000 px photo, where the
old rule gave 24). The guide of a RAW is its sRGB-encoded lightness; it was
the linear decode as 8 bits, which flattened edges in the shadows.

Measured in the app's code (`print_sam_mask_quality`: the same 500
DUTS-TE images and prompts as above, mask threshold 128 for IoU and boundary
F at 3 px, mean absolute error of the soft mask against the ground truth).
Click: IoU / boundary F / MAE; box the same:

| Refinement | Click | Box |
|---|---|---|
| none (decoder's hard mask) | 0.666 / 0.662 / 0.063 | 0.864 / 0.857 / 0.026 |
| 8-24 px (before) | 0.637 / 0.609 / 0.075 | 0.828 / 0.790 / 0.044 |
| 0.5 cell | 0.666 / 0.664 / 0.063 | 0.865 / 0.863 / 0.027 |
| **1 cell (now)** | **0.666 / 0.664 / 0.063** | **0.865 / 0.863 / 0.027** |
| 2 cells | 0.664 / 0.658 / 0.066 | 0.863 / 0.855 / 0.031 |
| 3 cells | 0.660 / 0.652 / 0.068 | 0.858 / 0.846 / 0.034 |

The same photos fed as linear decodes (the RAW path): 1 cell 0.662 / 0.657
and 0.864 / 0.860; 8-24 px with the old linear guide 0.630 / 0.594 and
0.822 / 0.775. On 200 of them enlarged four times (1200-1600 px, linear;
an enlargement has softer edges than a real photo of that size) the rules
come together: 1 cell 0.727 / 0.707 and 0.885 / 0.858, 8-24 px (12 px
here) 0.725 / 0.705 and 0.882 / 0.854, none 0.726 / 0.703 and 0.883 /
0.847. The refinement costs 20 ms per mask at 400 px and 95 ms at 1600 px
(CPU, M5 Pro, other load).

Above a long edge of 2048 px the final mask and the refinement's
coefficients run on a grid that many times coarser, and the coefficients
are applied to the full-size photo (He and Sun's fast guided filter); the
decoder's first pass, which only places the second pass's points, runs at a
long edge of at most 1024 px. On 80 of the photos enlarged six times
(1800-2400 px, linear) this is equal to the full-size refinement within
noise, also on coarser grids: click / box IoU and boundary F 0.748 / 0.738
and 0.883 / 0.871 at full size, 0.748 / 0.738 and 0.883 / 0.870 on the
2048 grid, 0.748 / 0.738 and 0.882 / 0.870 on a 512 grid. The low-res first
pass changed the 4x numbers above by at most +0.002. On single photos the
second pass's points can still land elsewhere and change which part an
ambiguous click selects (a click on a toy figure's body: with the head
before, without it after).

### Speed in the app

Time from a native click or box (QA control channel) to the mask in the
recipe (`maskDataBase64` in the editor store), QA build on an M5 Pro, RAW
QA copies of six cameras, one click per layer about a second after the
Object layer was created (`run_timing.sh`, numbers of one run each):

| | Before (encode on the first prompt, all AI models loaded, full-size mask) | After |
|---|---|---|
| 45 MP Canon R5, first mask after the launch | 5.44 s | 0.20 s |
| 45 MP, next masks on the photo | 4.28-4.41 s | 0.17-0.19 s |
| 40 MP Fujifilm (portrait), first on the photo | 4.55 s | 0.17 s |
| 24 MP Sony (portrait), box | 1.98 s | 0.17 s |
| 21 MP Nikon (portrait) | 1.81 s | 0.17 s |
| 12 MP iPhone ProRAW (linear DNG) | 0.98 s | 0.13 s |

The click 0.15 s after creating the layer (worst case for the background
work): before 1.7-7.2 s, after 0.13-0.18 s. What changed: SAM loads on its
own (64 MB, 0.5-0.9 s; before, the first object mask waited for BiRefNet,
sky and depth as well); the open photo is encoded in the background 1.5 s
after it is ready, if the model is installed (nothing is downloaded), and
at once when an Object layer becomes active; a prompt that arrives during
that encode waits for it instead of encoding again; the first decoder pass
runs at 1024 px (its distance transform at 45 MP took about 2 s); the mask
is refined on a grid of at most 2048 px (the feather was a single-threaded
blur at full size). "Before" is the same branch before these changes (the
pipeline of main plus the new refinement radius). Lightroom Classic 15.6 in
a separate side-by-side test on this Mac: first mask on a photo
0.33-0.55 s, next masks about 0.2 s, first after the launch 2.1 s; box mask
IoU there Rawtonic 0.924, Lightroom 0.885.
Costs of the background encode: 0.6-1 s of CPU per photo that stays open
1.5 s in Develop.

## SAM export quality

(SAM ViT-B, the model before SAM 2.1; `export_sam2.py` has the same kind of
check: `check_decoder()` fails unless the full-size masks are the low-res
masks scaled to 3:2, 4:3, 2:3, 1:1 and 16:9.)

Upstream's `SamOnnxModel` crops the upscaled mask with Python integers taken
from `orig_im_size`; the current PyTorch tracer stores them as constants of
the example input, so a plain export misplaces masks for every aspect ratio
but the example's (IoU 0.03 for portrait, 0.00 for square). The inherited
decoder does not have this fault. `export_sam.py` crops with tensor indices
and `check_decoder()` fails the export unless full-size and low-res masks
agree for 3:2, 4:3, 2:3, 1:1 and 16:9 (IoU 1.000 each).

Against the unquantised export of Meta's checkpoint, on 14 photos (3:2, 4:3
and portrait) with 84 random point/box prompts (mask 0, the one the app
uses), mask IoU:

| Export | Size (enc + dec) | Median IoU | Share > 0.9 |
|---|---|---|---|
| Inherited | 100.3 + 8.8 MB | 0.876 | 0.46 |
| Rawtonic, per-channel uint8 | 100.7 + 4.8 MB | 0.922 | 0.58 |

Encoder time on the CPU is unchanged (3.3 s vs 3.45 s per photo, M5 Pro, with
other load). Two builds produced the same encoder bytes.

## Hosting

The pack is the release `pack-2026-10-18` of
[github.com/rawtonic-app/models](https://github.com/rawtonic-app/models)
(README, `models.json`, `licenses/`), public together with Rawtonic. The app
downloads `MODEL_PACK_URL/<file>`
(`src-tauri/src/ai_processing.rs`) and the opt-in runtime build the same
release (`src-tauri/build.rs`); a download replaces an installed file only
after its SHA-256 matched. To install the whole pack ahead (for example
for QA builds), use
`scripts/models/install_models.sh [--runtime] [--cuda] [cc.rawtonic.editor|cc.rawtonic.qa]`
(needs `gh`).

A new pack is a new release (new tag): build it with
`build_model_pack.py`, upload every top-level file as an asset, then change
`MODEL_PACK_URL`, the SHA-256 constants and `install_models.sh`'s tag in one
commit. `build_model_pack.py --previous DIR` carries an earlier pack's
files (downloaded with `gh release download`) over unchanged, checked
against its `models.json`, and builds only what it lacks; that keeps the
files older builds load. `pack-2026-10-18` is `pack-2026-10-08` plus
the mixed package (built that way; `nlmf_mixed640_macos13_coreml_*` added
to it the same day); `pack-2026-10-08` stays
unchanged for the builds that point to it. Before that, new models went
into the existing release `pack-2026-10-08` as added assets with an
updated `models.json` (BiRefNet, sky, SAM 2.1). Check SAM with the ignored tests
`subject_masks_follow_the_box_in_every_aspect_ratio` (`AI_MODELS`,
`ORT_DYLIB_PATH`) and `print_sam_mask_quality` (also `SAM_EVAL`, a list of
`image;mask` lines).

## Measurements for the open slots (2026-10-08)

- **AI Denoise (NIND).** 17 high-ISO raw.pixls.us files (ISO 1600-32000),
  NIND run on Rawtonic renders without noise reduction (sRGB input, as
  trained), measured with notes: scripts/reference/editor_quality.py against
  Camera Raw's default (median ratio; lower noise is cleaner):

  | | no NR | Rawtonic NR (default) | NIND | Capture One default |
  |---|---|---|---|---|
  | fine / medium / coarse noise | 1.15 / 1.16 / 1.12 | **0.24 / 0.26 / 0.47** | 0.26 / 0.45 / 0.73 | 0.26 / 0.37 / 0.63 |
  | colour noise | 3.26 | **0.38** | 0.75 | 0.66 |
  | detail | 1.08 | 0.82 | 0.83 | 0.82 |

  At the same detail Rawtonic's own noise reduction removes more noise at
  every scale; in crops NIND leaves visible grain and, at ISO 32000, keeps
  edges crisper with a painterly look and shifted colours. NIND is not worth
  its unclear license. A raw-domain denoiser (RawNIND, Nonlocal MatchFilter)
  is the remaining candidate to beat it.
- **Raw-domain AI denoise (Nonlocal MatchFilter, MIT).** 13 high-ISO Bayer
  files, the model run on the CFA data of Adobe-converted uncompressed DNGs
  (blind shot/read noise map, 320 px tiles), all variants rendered by
  Rawtonic, median ratio to Camera Raw's default:

  | | Rawtonic NR | MatchFilter + Rawtonic colour NR | Capture One |
  |---|---|---|---|
  | fine / medium / coarse noise | 0.27 / 0.26 / **0.40** | **0.11 / 0.22** / 0.66 | 0.25 / 0.34 / 0.64 |
  | colour noise | **0.38** | 0.44 | 0.70 |
  | detail | 0.77 | 0.78 | 0.74 |
  | false colour | **0.58** | 0.86 | 1.11 |

  In crops MatchFilter is clearly best at ISO 32000 (crisp grass, clean
  shadows, where Rawtonic's NR leaves dark specks) and keeps texture at ISO
  6400-12800 with a slight worm-like pattern. Bayer only (no X-Trans). Speed
  on the M5 Pro: about 30 s per 24 MP photo with Core ML (0.22 s per tile);
  ONNX Runtime on the CPU 3.3 s per tile, its Core ML provider fails on the
  graph (notes: benchmark-results/raw-denoise-matchfilter-2026-10-08.json).
  In the app since 2026-10-08 as AI Denoise (`crates/denoise`).
  Linux, 2026-10-18 (Ryzen 5 3600X, RTX 3080, NVIDIA 615, ONNX Runtime
  1.30.0; four public raw.pixls.us files, 24-44 MP; `cargo run -p
  rawtonic-denoise --example denoise_bench`): the CPU provider takes
  431-437 s per 24 MP and 762 s for 44 MP (about 3.7 s per tile, peak
  6.7-7.1 GB); the bundled WebGPU plugin on Vulkan 35.4-35.6 s per 24 MP and
  60.6 s for 44 MP (12×, peak 3.5 GB, about 4.3 GB of GPU memory), within
  one f16 step of the CPU's result (largest difference 4.9e-4 of white, mean
  ≤ 4.8e-8, ≤ 0.07 % of samples). The app chooses WebGPU there by itself;
  a headless export of a 24 MP photo took 39 s (3.7 s from the cache).
  More CPU threads or two tiles at once did not help (≤ 6 %, twice the
  memory). The CUDA plugin was not measured: it needs CUDA 13 and cuDNN 9,
  which the machine does not have
  (notes: benchmark-results/ai-denoise-linux-2026-10-18.json).
- **AI Denoise versions 2 and 3 (2026-10-18, macOS, M5 Pro; notes:
  benchmark-results/ai-denoise-mac-2026-10-18.json, task DN).** `aiDenoiseVersion`
  2 is the new slider on the first package, 3 the same slider on the
  mixed-precision 640 px package (the default for new RAW edits since
  pack-2026-10-18 carries it; no version falls back to another model). Version 1 recipes render as before
  (a fresh version 1 result is byte-identical to main's):
  - *Slider.* The slider now sets the noise the network is told about
    instead of only blending its result: at 50 the full result at 0.8 × the
    measured noise, above 50 up to 1.25 × (75 = version 1's 100), below 50
    a blend. On six RawNIND ISO 12800+ scenes (each editor against its own
    render of the clean frame, Oklab ×100, geometric means) version 2 at 50
    leaves flat-area noise 1.065 and keeps 0.894 of the finest detail
    (0.7–2 px), Camera Raw Denoise at its default 50 1.06 and 0.901 — the
    same effect on the scenes the 0.8 was chosen on; on seven held-out
    RawNIND scenes (Bark, Kortlek, sewingmachine, LucieB tapestry, A7C goat,
    TitusToys, 7D-4; ISO 6400–25600; Camera Raw re-run) version 2 at 50 is a
    little stronger: flat noise 1.16 vs 1.30, detail 0.748 vs 0.778, error
    2.48 vs 2.05 (version 1 at 50: 1.05 / 0.623 / 2.50). Version 1 at 50
    left 0.995 and kept 0.825 on the first six. At equal
    flat-area noise a noise scale keeps more detail than blending (e.g.
    flat noise 0.81: detail 0.871 against 0.862, error 2.0 against 2.11).
    The remaining gap is the network's: Camera Raw's lightness error at 50
    is 1.69, version 2's 2.21 at the same noise and detail (1.79 at 75);
    with sharpening 0 in both editors 1.17 against 1.71 (detail 0.93 against
    0.92, flat noise 0.63 against 0.79).
  - *Precision.* `nlmf_mixed640_coreml_*` computes the convolutions in
    float16 and the feature matching and sampling in float32
    (`convert_nlmf_coreml.py`). All-float16 is 3× faster but loses 2 % of
    the finest detail's energy (detail kept −0.02 on all six scenes); the
    mixed program keeps it (0.01 noise σ RMS from float32 on real tiles; the
    six-scene numbers equal float32's to three decimals) at 0.84 µs per
    sample instead of 2.2.
  - *Tiles.* 640 px tiles (560 px kept) instead of 320 (240 kept): 1.4× fewer
    samples through the network per photo. Two tiles at once on the GPU
    gave nothing (2.93 tiles/s either way); the Neural Engine runs the
    mixed program 4× slower than the GPU.
  Speed of version 3 (export of the four G18 files, new process, empty cache; three
  repeats, machine shared): 24 MP 9.7–10.9 s instead of 26.8–30.6 s
  (MatchFilter 8.5–9.5 s instead of 25.5–29.1 s), 44 MP 16.9–18.8 s
  instead of 48–53 s, the four in one process 50.5–51.7 s instead of
  143–145 s; peak memory about 0.1 GB higher (1.4–2.5 GB). The first load
  compiles the package (D7200: 9.9 s in all). Camera Raw Denoise takes
  6.3–6.9 s per 24 MP (command to result), so version 3 is still about
  1.5× slower; version 2 runs at version 1's speed.
  Candidates measured for better fine texture, all with weights under MIT
  from their authors, on the same six scenes through Rawtonic's pipeline:
  DRUNet (KAIR, MIT) on the packed mosaic after a variance-stabilising
  transform, per plane or as RGB pairs: detail kept 0.69 / 0.73 against
  MatchFilter's 0.83 at the same flat-area noise (scene 46), colour error
  higher; NAFNet-SIDD width 32 (MIT) on Rawtonic's render without noise
  reduction (sRGB, as trained): detail 0.71–0.83 with more residual noise
  than MatchFilter (three scenes, sharpening 0). Not measured: Restormer,
  KBNet, Uformer (sRGB SIDD models like NAFNet, 5–10× its cost), SCUNet,
  SwinIR (synthetic Gaussian/blind sRGB), PMRID (raw, Apache-2.0, small and
  older), ELD/PMN/SID (one camera, extreme low light); excluded by license:
  LED, DualDn (CC BY-NC), MIRNet-v2/CycleISP (academic), vkdt's jddcnn
  (weights without a license). The blind noise estimate is within 5–10 % of the true
  noise (RawNIND pairs, slightly low), so it does not explain the gap.
- **RawNIND joint denoise-demosaic network (2026-10-18, macOS, M5 Pro;
  notes: benchmark-results/ai-denoise-mac-2026-10-18.json, task DN2).**
  trougnouf/rawnind_jddc (GPL-3.0 code, weights "GPL3 + CC-BY-4.0"), the
  Bayer-to-camera-RGB denoisers `2024-02-21-bayer` (32 channels) and
  `2024-08-14-f48` (48), measured only, not in the app. Its camera-RGB
  result was sampled back onto the mosaic and rendered by Rawtonic's
  pipeline in place of MatchFilter's (same metric and scenes as above;
  version 2/3 at 50 reproduced the earlier numbers exactly). At full
  strength it is much smoother than Camera Raw 50 and loses fine detail:
  fitted six scenes error 1.97 / finest detail kept 0.72 / flat-area noise
  0.46 (Camera Raw 50: 1.69 / 0.90 / 1.06; MatchFilter v3 50: 2.21 / 0.89 /
  1.07); seven held-out scenes 2.05 / 0.59 / 0.48 (Camera Raw 2.05 / 0.78 /
  1.30; v3 2.48 / 0.75 / 1.16). Blended to Camera Raw's flat-area noise
  (half strength) it is worse than MatchFilter on every count (fitted
  2.32 / 0.78 / 0.98, held-out 2.49 / 0.58 / 1.01); the 48-channel model is
  no better. That although 12 of the 13 scenes are in its training data
  (only TitusToys is in its test reserve), which favours it. It is fast:
  0.9 s per 24 MP and 1.4 s per 44 MP (PyTorch on the GPU, network only;
  MatchFilter v3 about 9 s). Below the bar on fine detail, so not adopted;
  the weight policy stays as it is. Not covered: its own demosaic (the
  result went through Rawtonic's), colour noise beyond err_ab.
- **Auto tags.** On 14 photos SigLIP 2 B/16 ranked people and scenes better
  than CLIP (a grandfather and child at a table: "people, patio, child,
  balcony, senior, family, table" vs CLIP's "table, hygge, camera, remote"; a
  balance bike: "boy, bicycle, cycling, child" vs "scooter, cycling,
  motorcycle"). In the app since 2026-10-08: score = sigmoid(112.67 · cosine
  − 16.77) (SigLIP's own calibration), a tag is kept at ≥ 0.1 × the best
  score (and ≥ 0.0005), up to the tag count setting. "self-portrait" was
  dropped from the tag list (it fired on every portrait; nothing in the
  pixels says who pressed the shutter). The 589 built-in tags are
  precomputed (`siglip2_tags.bin`, 1.8 MB); the 1.1 GB text encoder loads
  only for the user's own tags, whose embeddings are computed once per
  folder (the inherited code encoded the whole tag list for every photo).
  Vision encoder 372 MB in fp32: dynamic uint8 quantisation breaks it
  (embedding cosine 0.80).
- **Object removal.** MI-GAN (28 MB) against the inherited LaMa on two
  SAM-masked removals: on a dog in grass both are clean (MI-GAN leaves one
  small speck); on a person against a wall LaMa fills smoothly and MI-GAN
  invents dark shapes. On three photos of 33-61 MP (cars on a road, a
  stamp on a white cap, a chart marker) both are clean; on a twig crossing a
  bottle and a tomato MI-GAN paints a brown-blue blot where LaMa smears.
  MI-GAN runs in 0.2-0.9 s on the CPU, LaMa in 14 s. MI-GAN is the clean
  license, LaMa the better result on large holes across object edges; the
  app uses MI-GAN since 2026-10-09. Generative fill (FLUX.2 [klein] 4B) is
  the candidate for the hard cases.

  Broader check (2026-10-09): 16 RAW samples (2048 px exports), random
  brush strokes covering 0.5 %, 2 % and 6 % of the photo, fill compared with
  the original pixels (median Lab difference in the hole, lower is closer):

  | Hole | LaMa | MI-GAN (app) | MI-GAN, 3 passes | MI-GAN, 5 passes |
  |---|---|---|---|---|
  | 0.5 % | 2.70 | 2.68 | – | – |
  | 2 % | 2.08 | 1.91 | 1.80 | 1.68 |
  | 6 % | 3.45 | 4.11 | 3.83 | 3.61 |
  | time | 14.2 s | 0.12 s | 0.4 s | 0.7 s |

  MI-GAN matches or beats LaMa up to medium holes; on large ones LaMa's
  fills are visibly more coherent. Filling large holes from the edge inwards
  in several passes lowers the number but paints streaks and repeated
  texture (rock, foliage, brickwork), so the app keeps one pass. MI-GAN's own
  512 px generator on a wider crop (2-3 × the hole) was worse than the
  pipeline. For comparison, Capture One has no AI object removal (heal and
  clone, AI dust removal); Lightroom's Generative Remove (Firefly, in the
  cloud) is the reference for large holes.

  **Remove's fill rebuilt (2026-10-10).** Lightroom-level fills at
  junctions (wall, window and roof meeting) were the open gap: MI-GAN works
  at 512 px, so a large hole is upscaled several times and comes out soft,
  with smeared tiles and dark blots. Test set: 24 objects in 22 CC0
  raw.pixls.us photos (2-45 MP, rendered by the `rawtonic` CLI; bins, poles,
  cars, people, a parasol, a boat, a scaffold tube), each selected the way
  Detect objects does it (SAM 2.1 two-pass mask, grown 0.5 % and feathered
  0.25 % of the short side). For a known answer every object's mask shape
  was also moved onto the most structured of 64 places in another photo;
  LPIPS (AlexNet), PSNR and mean Lab ΔE of the hole there, the hole's
  high-pass detail against the photo's (1 = as sharp as the photo), time per
  fill on an M5 Pro CPU (other load on the machine). Harness:
  `scripts/models/eval_inpaint.py`, `crates/engine/examples/patch_fill.rs`.

  | Method | Weights license | LPIPS ↓ | PSNR ↑ | ΔE ↓ | Detail | Time (median, real cases) |
  |---|---|---|---|---|---|---|
  | MI-GAN pipeline (the app before) | MIT | 0.0732 | 15.30 | 17.87 | 0.56 | 0.14 s |
  | MI-GAN, hole = mask ≥ 50 % only | MIT | 0.0634 | 16.03 | 15.73 | 0.61 | 0.16 s |
  | MI-GAN generator alone, square crop 2 × the hole | MIT | 0.0765 | 15.78 | 15.90 | 0.49 | 0.32 s |
  | MI-GAN in overlapping 320 px tiles | MIT | 0.0682 | 15.69 | 15.95 | 0.61 | 1.2 s |
  | MI-GAN + matched grain | MIT | 0.0616 | 15.87 | 16.04 | 0.85 | 0.19 s |
  | HiFill (Huawei, contextual residual aggregation) | BSD-3 (in its repo) | 0.0760 | 15.14 | 18.05 | 0.61 | 1.0 s |
  | LaMa (big-lama, 512 px; evaluation only) | none stated | 0.0763 | 16.54 | 14.65 | 0.39 | 2.3 s |
  | Patch completion alone (own, no model) | – | 0.0853 | 15.17 | 18.76 | 0.52 | 1.5 s |
  | **MI-GAN-guided patch completion (now)** | MIT + own | **0.0704** | 15.47 | 17.38 | 0.53 | 1.7 s |
  | the same, hole = mask ≥ 50 % | MIT + own | 0.0619 | 16.22 | 15.44 | 0.58 | 1.7 s |

  The "≥ 50 %" rows look best by the numbers because the synthetic holes'
  feathered rim is then filled with the true pixels; on real objects that
  rim holds the object's edge and shadow, which MI-GAN grew back into
  shapes of the object (a dark bin where a bin was, a smeared ladder), so
  the app fills everything the selection touches. Matched grain helps the
  numbers but adds noise over MI-GAN's blots instead of removing them.
  On the real cases (crops judged side by side) the guided completion
  replaces MI-GAN's smears with the photo's own texture: paving, grass,
  brick and water continue sharp and with grain (a boat on water, a person
  on a street, a pinwheel against foliage are clean); at hard junctions it
  still copies wrong structure (a fence and a pillar running into the hole
  behind a wheelie bin, a car's shadow over brick paving). LaMa is no better
  there and has no weight license; HiFill is older and weaker; MAT, ZITS,
  FcF, CoModGAN, AOT-GAN, LDM inpainting and every SD/SDXL/FLUX.1-based
  remover fail the policy (non-commercial, RAIL or no statement).

  Generative: FLUX.2 [klein] 4B (Apache-2.0; the Qwen3-4B text encoder
  Apache-2.0 too) as an editing model, run through diffusers on Apple MPS
  (bf16, 4 steps, a 1024 px square crop 2 × the object, the prompt "Remove
  <the object> and show the background behind it", the result composited
  with Remove's mask), on five of the cases: the bin, the boat, the parasol
  and the pinwheel are removed cleanly and the scene behind is continued
  (fence, pillar, shrub and kerb behind the bin; Lightroom-Generative
  level), the pole was not. Cost: 22 GB download (bf16 transformer + text
  encoder + VAE), about 13 GB of memory, 135-670 s per removal on this M5
  Pro under heavy load from other work (it pushed the machine out of
  memory once). Not integrated: no ONNX export yet, far too slow and large
  as the default fill, and it needs a text prompt per object (Lightroom's
  Generative Remove needs none). It is the candidate for an optional
  "generative" fill (quantised, ONNX, downloaded on request), the open part of G33.
  Lightroom Classic's Generative Remove (Firefly, in the cloud) on the same
  CC0 photos is
  clearly better at junctions (fence, pillar and grass continued behind
  the bin; railing and wall behind the car), 4.8-7 s per removal; its local
  Remove smears like ours (a duplicated mailbox and brown blots at the bin,
  repeated paving chunks and a ghost ladder at the car), about 4 s. Rawtonic's
  Remove: 1.3 s for the bin on the 9 MP photo in the QA app (selection +
  fill, clicked natively over the QA control channel). The fill alone
  (ignored test `print_removal_fill_times`, M5 Pro without other load):
  0.27-0.44 s for a person and the bin (3-9 MP), 1.0 s for the car (21 MP),
  1.3-1.5 s for the pinwheel and a lemon covering a third of a 45 MP photo;
  MI-GAN alone took 0.18-0.52 s. Holes above 400 000 pixels only refine
  their finest levels (one local PatchMatch pass where the hole is wider
  than 256 of a level's pixels); the table above ran without that budget.
  Lightroom's times were taken while the machine was out of memory from
  other work and are an upper bound.
- **Sky.** UperNet + ConvNeXt-T against the inherited sky model on 34 RAW
  samples (22 with sky): solid masks on overcast and cloud-textured skies
  where the old one had holes, a missed sky found (bare trees in snow), no
  false sky on a white wall, and photos without sky stay below 0.08 (the old
  code min-max scaled noise into a mask). Weaker: the network predicts at a
  quarter of 512 px, so thin structures and sky between leaves are soft; the
  app refines the mask with a guided filter on the photo (radius 1/50 of a
  1024 px copy, eps 1e-3, then levels 0.25-0.75), which restores spires,
  lamp posts and tree outlines (since 2026-10-10 the edge band is also
  decided by colour, see "Cold Subject and Sky, Sky edges"). 0.72 s per
  photo on the CPU (old: 0.2 s);
  119 MB (fp16 storage; fp32 237 MB, same masks; dynamic uint8 rejected,
  IoU down to 0.47).

## Candidates for the open slots

| Slot | Candidate | License | Notes |
|---|---|---|---|
| Generative fill | FLUX.2 [klein] 4B + Qwen3-4B text encoder | Apache-2.0 | Measured 2026-10-10 (Object removal): clean removals with a prompt naming the object, Lightroom-Generative level at junctions; 22 GB bf16, ~13 GB memory, 135-670 s per removal on a loaded M5 Pro; needs a quantised ONNX export |
| Object removal (generative) | LensErase (Microsoft and universities; github.com/LensFamily/LensErase, arXiv 2610.18072) on the Lens 3.8B diffusion model | Repository and Lens code MIT; the weights are not yet published (checked 2026-10-14), their license is unknown | Mask-only removal (no prompt) that also removes shadows and reflections, which is what Remove lacks at junctions. Once released, check: the weight license on its model card, the FLUX.2 VAE it uses (Apache-2.0 in [klein] 4B), whether the GPT-OSS text encoder can be left out (precomputed empty prompt), size and speed on Apple Silicon (bf16 ~7.6 GB for the denoiser), crop-and-blend into full-resolution RAWs; then measure against MI-GAN, the patch fill and FLUX.2 [klein] 4B |
| Object removal | HiFill (Huawei) | BSD-3 (weights in the authors' repository) | Measured: weaker than MI-GAN; not used |
| AI denoise | RawNIND weights (Brummer) | GPL-3.0 + CC BY 4.0 (stated) | Bayer and linear-RGB denoisers; no ONNX yet |

Excluded for their weight licenses: FLUX.1 Fill/Kontext [dev], FLUX.2 [dev]
and [klein] 9B, MAT, Qwen-Image-2.1, Mask2Former and SegFormer weights,
Apple MobileCLIP/DFN, MetaCLIP, CycleISP, MIRNet-v2, LED, DualDn,
Noise2Noise.

### Relight people normals

`relight_people_normals_david_base.onnx` is 449,232,211 bytes, SHA-256
`3d37ecc812b6b50a09c514f3c8c8ad95da5cd6ae9b68c425bc52f1c9cd78c344`, the
file Microsoft publishes (an fp16 conversion fails on its Cast nodes). Input
is float32 `[1,3,512,512]`, BGR in 0–1, the subject's region padded to a
square by repeating its border (as DAViD's runtime); output is a surface
normal per pixel (x right, y up, z towards the camera). It is loaded and
run only for photos with people (a detected face, or skin on at least
1/300 of the frame in the whole-photo parse) and a subject matte, on the
matte's region (BiRefNet), in
0.26 s on an M5 Pro CPU. Measured on the public relight set (notes:
reference/relight-g70-close.md): its Base and Large normals differ by
7.4° mean on a portrait, with the same wrinkle and nose detail; it is
human-only by design (the app uses scene depth for everything else).

### Relight camera geometry

`relight_geometry_da3_base.onnx` is 412,844,389 bytes, SHA-256
`070a4ff34ed406d057044daf9b84fd6287e5a7cc66134060acf2ccfe5d5dea23`.
The original safetensors checkpoint is SHA-256
`e01067dc1659613083d9145a9a2547ccdbe6ccbbf83c4fe7b3e8a4e2bdae78b5`.
The pack's exporter pins the source, checkpoint, config and conversion environment.
The export patch fixes symbolic spatial sizes, positional interpolation and UV
aspect ratios; it retains the original network and weights. The ONNX output
was compared with unmodified Torch at ten aspect sizes on runtime 1.23.2.

Input is float32 `[1,1,3,H,W]`, ImageNet-normalized sRGB, resized without padding
to a 518-pixel longest side and multiples of 14, minimum 28 per side. The
minimum avoids undefined single-token camera geometry on extremely narrow
frames. Output is relative camera-axis
distance and intrinsics, not calibrated metres. The app normalizes by median
distance, stores 16-bit inverse depth plus the camera in version-4 and
version-5 recipes, and
fits a cached surface for GPU shading. Bounds on extreme inverse distance are
0.0001–100 after normalization. Existing maps retain their previous renderer;
new analysis is explicit and light movement performs no model inference.

Controlled geometry and runtime comparisons are recorded in the private notes
(reference/relight-da3.md). Single-image geometry remains uncertain at hair,
thin edges, glossy/transparent products and defocused backgrounds; this is not
recovered ground-truth geometry or material/source-light separation.

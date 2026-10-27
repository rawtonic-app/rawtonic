// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * About: the legal notices the AGPL asks an interactive program to show
 * (copyright, no warranty, the license and where the source is), the
 * notices of the DNG patent license and libjpeg-turbo, trademarks, and the
 * credits for the projects, data and models Rawtonic uses.
 */
import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-shell";
import { Invokes } from "../ui/AppProperties";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";

export const SOURCE_URL = "https://github.com/rawtonic-app/rawtonic";
const COMMIT: string | undefined = import.meta.env.VITE_RAWTONIC_COMMIT;

interface Credit {
  name: string;
  url: string;
  what: string;
  license: string;
}

export const CREDITS: Credit[] = [
  {
    name: "RapidRAW",
    url: "https://github.com/CyberTimon/RapidRAW",
    what: "The open-source RAW editor by Timon Käch that Rawtonic's editor derives from (commit a82251f).",
    license: "AGPL-3.0",
  },
  {
    name: "rawler",
    url: "https://github.com/dnglab/dnglab",
    what: "RAW decoding (dnglab, via RapidRAW-DngLab), with Rawtonic patches; Sony ARW 6 ported from LibRaw.",
    license: "LGPL-2.1",
  },
  {
    name: "Lensfun database",
    url: "https://lensfun.github.io/",
    what: "Lens profiles for automatic lens correction (data only; no Lensfun code).",
    license: "CC BY-SA 3.0",
  },
  {
    name: "spektrafilm",
    url: "https://github.com/andreavolpato/spektrafilm",
    what: "Film emulation LUTs by Andrea Volpato (renamed, contents unchanged).",
    license: "CC BY-SA 4.0",
  },
  {
    name: "NegPy",
    url: "https://github.com/marcinz606/NegPy",
    what: "Inspiration for the negative conversion.",
    license: "GPL-3.0",
  },
  {
    name: "darktable",
    url: "https://github.com/darktable-org/darktable",
    what: "Reference implementations that guided parts of the inherited editor.",
    license: "GPL-3.0",
  },
  {
    name: "SAM 2",
    url: "https://github.com/facebookresearch/sam2",
    what: "Click and box masks (SAM 2.1 Hiera-T, Meta).",
    license: "Apache-2.0",
  },
  {
    name: "BiRefNet",
    url: "https://github.com/ZhengPeng7/BiRefNet",
    what: "Subject and background masks (Peng Zheng et al.).",
    license: "MIT",
  },
  {
    name: "ConvNeXt UperNet",
    url: "https://github.com/facebookresearch/ConvNeXt",
    what: "Sky masks (Meta, ADE20K).",
    license: "MIT",
  },
  {
    name: "Depth Anything 3 Base",
    url: "https://github.com/ByteDance-Seed/Depth-Anything-3",
    what: "Relative scene geometry and camera estimation for Relight (ByteDance Seed).",
    license: "Apache-2.0",
  },
  {
    name: "DAViD",
    url: "https://github.com/microsoft/DAViD",
    what: "Surface normals of people for Relight (Microsoft).",
    license: "MIT",
  },
  {
    name: "Depth Anything V2 Small",
    url: "https://github.com/DepthAnything/Depth-Anything-V2",
    what: "Depth masks.",
    license: "Apache-2.0",
  },
  {
    name: "YuNet",
    url: "https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet",
    what: "Finds faces for People masks (Shiqi Yu et al.).",
    license: "MIT",
  },
  {
    name: "MediaPipe Face Mesh V2 and selfie segmentation",
    url: "https://ai.google.dev/edge/mediapipe/solutions/vision/face_landmarker",
    what: "Face landmarks, Relight mesh topology, and skin, hair and clothes for People masks (Google).",
    license: "Apache-2.0",
  },
  {
    name: "SigLIP 2",
    url: "https://huggingface.co/google/siglip2-base-patch16-224",
    what: "Automatic keywords (Google).",
    license: "Apache-2.0",
  },
  {
    name: "Nonlocal MatchFilter",
    url: "https://github.com/MIA-UIB/nonlocal-matchfilter",
    what: "AI Denoise on the RAW mosaic.",
    license: "MIT",
  },
  {
    name: "MI-GAN",
    url: "https://github.com/Picsart-AI-Research/MI-GAN",
    what: "Object removal (Picsart AI Research).",
    license: "MIT",
  },
  {
    name: "Moebius",
    url: "https://github.com/hustvl/Moebius",
    what: "Removal of large objects on macOS (Huazhong University of Science and Technology, vivo AI Lab).",
    license: "Apache-2.0",
  },
  {
    name: "SDXL-VAE-FP16-Fix",
    url: "https://huggingface.co/madebyollin/sdxl-vae-fp16-fix",
    what: "Image encoder and decoder of Moebius (madebyollin, from Stability AI's SDXL VAE).",
    license: "MIT",
  },
  {
    name: "ONNX Runtime",
    url: "https://github.com/microsoft/onnxruntime",
    what: "Runs the AI models, on Windows and Linux also on the GPU through its WebGPU plugin (Microsoft).",
    license: "MIT",
  },
  {
    name: "Dawn",
    url: "https://dawn.googlesource.com/dawn",
    what: "WebGPU for the AI models on Windows and Linux, inside ONNX Runtime's WebGPU plugin (Google).",
    license: "BSD-3-Clause",
  },
  {
    name: "libgphoto2",
    url: "http://www.gphoto.org/proj/libgphoto2/",
    what: "Tethered shooting: talks to the camera over USB, in a separate helper process (with libltdl, and libusb on Linux; linked dynamically). On macOS through ImageCaptureCore.",
    license: "LGPL-2.1-or-later",
  },
  {
    name: "libgphoto2 Rust bindings",
    url: "https://github.com/CyberTimon/libgphoto2-rs",
    what: "The tether helper's bindings to libgphoto2 (RapidRAW's fork of vibe-libgphoto2-rs).",
    license: "MIT OR Apache-2.0",
  },
  {
    name: "libjpeg-turbo",
    url: "https://libjpeg-turbo.org/",
    what: "JPEG export.",
    license: "IJG, BSD-3-Clause, zlib",
  },
  {
    name: "moxcms",
    url: "https://github.com/awxkee/moxcms",
    what: "Reads the ICC profiles embedded in JPEG, PNG, TIFF, WebP and AVIF sources.",
    license: "BSD-3-Clause OR Apache-2.0",
  },
  {
    name: "rav1d",
    url: "https://github.com/memorysafety/rav1d",
    what: "Decodes AVIF sources (the Rust port of dav1d).",
    license: "BSD-2-Clause",
  },
  {
    name: "avif-parse",
    url: "https://github.com/kornelski/avif-parse",
    what: "Reads the AVIF container.",
    license: "MPL-2.0",
  },
];

function Link({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      onClick={(event) => {
        event.preventDefault();
        void open(href);
      }}
      className="font-semibold text-accent hover:underline"
    >
      {children}
    </a>
  );
}

export default function AboutSettings() {
  const [version, setVersion] = useState<string | null>(null);
  const [licenses, setLicenses] = useState<string | null>(null);
  const showLicenses = () => {
    invoke<string>(Invokes.ThirdPartyLicenses)
      .then(setLicenses)
      .catch((error) => setLicenses(String(error)));
  };
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(null));
  }, []);

  return (
    <>
      <div className="pref-section">
        <Text
          as="h2"
          variant={TextVariants.title}
          className="pref-section-title"
        >
          Rawtonic {version ?? ""}
        </Text>
        <div className="space-y-3">
          <Text>
            Copyright © 2026 Rawtonic contributors. Portions copyright Timon
            Käch and the RapidRAW contributors.
          </Text>
          <Text>
            This program is free software: you can redistribute it and/or modify
            it under the terms of the GNU Affero General Public License as
            published by the Free Software Foundation, either version 3 of the
            License, or (at your option) any later version. It is distributed in
            the hope that it will be useful, but{" "}
            <strong>without any warranty</strong>; without even the implied
            warranty of merchantability or fitness for a particular purpose. See
            the{" "}
            <Link href="https://www.gnu.org/licenses/agpl-3.0.html">
              GNU AGPL
            </Link>{" "}
            for details; a copy ships with the app.
          </Text>
          <Text>
            Source code: <Link href={SOURCE_URL}>{SOURCE_URL}</Link>
            {COMMIT ? ` (this build: ${COMMIT})` : ""}
          </Text>
          <Text>
            This product includes DNG technology under license by Adobe.
          </Text>
          <Text>
            This software is based in part on the work of the Independent JPEG
            Group.
          </Text>
          <Text>
            Rawtonic is an independent project. It is not affiliated with,
            endorsed or sponsored by Adobe, Capture One, Fujifilm or any camera
            or software maker. Product names are trademarks of their owners and
            are used only to identify their products, for example the catalogs
            and presets Rawtonic can import.
          </Text>
          <Text>
            Photos are processed on this computer. Rawtonic has no accounts
            or telemetry. An optional startup update check contacts GitHub at most once
            per day; turn it off in Preferences. It otherwise goes online when you ask it to:
            to download an AI model the first time you use an AI tool, to show a
            photo&apos;s location on OpenStreetMap, or to send a photo to a
            generative AI server you set up yourself.
          </Text>
        </div>
      </div>
      <div className="pref-section">
        <Text
          as="h2"
          variant={TextVariants.title}
          className="pref-section-title"
        >
          Credits
        </Text>
        <Text className="mb-4">
          Rawtonic builds on these projects, data and models. The license texts
          of every third-party package the app includes and of the AI models
          it downloads are below.
        </Text>
        <Text as="ul" className="space-y-3 list-disc ml-5 pl-1">
          {CREDITS.map((credit) => (
            <li key={credit.name}>
              <Link href={credit.url}>{credit.name}</Link>: {credit.what} (
              {credit.license})
            </li>
          ))}
        </Text>
      </div>
      <div className="pref-section">
        <Text
          as="h2"
          variant={TextVariants.title}
          className="pref-section-title"
        >
          Third-party licenses
        </Text>
        {licenses === null ? (
          <button
            type="button"
            className="text-accent hover:underline"
            onClick={showLicenses}
          >
            Show the license texts
          </button>
        ) : (
          <pre
            aria-label="Third-party license texts"
            className="max-h-[480px] overflow-auto whitespace-pre-wrap text-xs"
          >
            {licenses}
          </pre>
        )}
      </div>
    </>
  );
}

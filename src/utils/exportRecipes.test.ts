import { expect, it } from "vitest";
import {
  DEFAULT_RECIPE_DRAFT,
  draftFromPreset,
  exportSettingsFromDraft,
  exportSettingsFromPreset,
  presetFromDraft,
  savedRecipes,
  subfolderName,
  subfolderProblem,
  type RecipeDraft,
} from "./exportRecipes";
import {
  BorderBasis,
  WatermarkAnchor,
  type ExportPreset,
} from "../components/ui/ExportImportProperties";

const draft: RecipeDraft = {
  ...DEFAULT_RECIPE_DRAFT,
  format: "tiff",
  quality: 80,
  depth: 8,
  colorSpace: "adobeRgb",
  resize: true,
  resizeValue: 2048,
  metadata: false,
  stripGps: true,
  template: "{orig_base}_web",
  where: "output",
};

const everything: RecipeDraft = {
  ...draft,
  format: "webp",
  resizeMode: "megapixels",
  resizeValue: 2.5,
  dontEnlarge: false,
  subfolder: "Web",
  preserveFolders: true,
  preserveTimestamps: false,
  exportMasks: true,
  watermark: true,
  watermarkPath: "/logo.png",
  watermarkAnchor: WatermarkAnchor.TopLeft,
  watermarkScale: 12,
  watermarkSpacing: 3,
  watermarkOpacity: 60,
  border: true,
  borderBasis: BorderBasis.EachEdge,
  borderHorizontal: 3.5,
  borderVertical: 1,
  borderColor: "#000000",
  pad: true,
  padWidth: 4,
  padHeight: 5,
  padColor: "#eeeeee",
  sharpening: "screen",
  sharpenAmount: 120,
  sharpenRadius: 0.6,
  sharpenThreshold: 2,
  printDistanceCm: 80,
  printPpi: 240,
};

it("keeps the form through a saved recipe", () => {
  const preset = presetFromDraft(draft, "r1", "Web");
  expect(preset).toMatchObject({
    id: "r1",
    name: "Web",
    fileFormat: "tiff",
    tiffBitDepth: 8,
    colorSpace: "adobeRgb",
    destinationType: "sessionOutput",
  });
  expect(draftFromPreset(preset)).toEqual({ ...draft, folder: undefined });
  const chosen = presetFromDraft(
    { ...draft, where: "choose", folder: "/out" },
    "r2",
    "Print",
  );
  expect(draftFromPreset(chosen)).toMatchObject({
    where: "choose",
    folder: "/out",
  });
});

it("keeps every export setting through a saved recipe", () => {
  const preset = presetFromDraft(everything, "r1", "Web");
  expect(preset).toMatchObject({
    fileFormat: "webp",
    resizeMode: "megapixels",
    resizeValue: 2.5,
    dontEnlarge: false,
    subfolder: "Web",
    preserveFolders: true,
    preserveTimestamps: false,
    exportMasks: true,
    enableWatermark: true,
    watermarkAnchor: "topLeft",
    enableBorder: true,
    borderBasis: "eachEdge",
    enablePad: true,
    padRatioWidth: 4,
  });
  expect(draftFromPreset(preset)).toEqual({ ...everything, folder: undefined });
});

it("gives recipes saved before these settings the form defaults", () => {
  // A recipe as the Studio saved it before: no watermark, border or folder fields.
  const old = {
    id: "old",
    name: "Old",
    fileFormat: "jpeg",
    jpegQuality: 85,
    enableResize: true,
    resizeMode: "longEdge",
    resizeValue: 2000,
    keepMetadata: true,
    stripGps: false,
    filenameTemplate: "{original_filename}",
    destinationType: "customFolder",
  } as unknown as ExportPreset;
  const d = draftFromPreset(old);
  // Its name template is read in the naming tokens, naming as before.
  expect(d.template).toBe("{orig_base}");
  expect(draftFromPreset({ ...old, filenameTemplate: "{YYYY}{MM}{DD}-{hh}{mm}_{sequence}" }).template).toBe(
    "{yyyy}{mm}{dd}-{hh}{MM}_{seq:auto}",
  );
  // Saved again, it holds the naming tokens; one saved since keeps them.
  const again = presetFromDraft(d, "old", "Old", old);
  expect(again).toMatchObject({ filenameTemplate: "", nameTemplate: "{orig_base}" });
  expect(draftFromPreset({ ...again, nameTemplate: "{job}/{mm}" }).template).toBe("{job}/{mm}");
  expect(d).toMatchObject({
    colorSpace: "srgb",
    dontEnlarge: true,
    preserveTimestamps: true,
    preserveFolders: false,
    subfolder: "",
    exportMasks: false,
    watermark: false,
    border: false,
    pad: false,
  });
  const { settings, outputFormat } = exportSettingsFromPreset(old);
  expect(outputFormat).toBe("jpg");
  expect(settings).toMatchObject({
    resize: { mode: "longEdge", value: 2000, dontEnlarge: true },
    preserveTimestamps: true,
    watermark: null,
    border: null,
    pad: null,
    exportMasks: false,
    preserveFolders: false,
    subfolder: "",
    outputSharpening: null,
  });
  // An older CUBE recipe keeps exporting LUTs.
  expect(
    exportSettingsFromPreset({ ...old, fileFormat: "cube" }).outputFormat,
  ).toBe("cube");
});

it("builds the export_images settings from the form", () => {
  const { settings, outputFormat } = exportSettingsFromDraft(everything);
  expect(outputFormat).toBe("webp");
  expect(settings).toEqual({
    filenameTemplate: null,
    nameTemplate: "{orig_base}_web",
    sequenceStart: 1,
    jpegQuality: 80,
    tiffBitDepth: 8,
    colorSpace: "adobeRgb",
    keepMetadata: false,
    preserveTimestamps: false,
    stripGps: true,
    keepCreatorContact: false,
    resize: { mode: "megapixels", value: 2.5, dontEnlarge: false },
    border: {
      basis: "eachEdge",
      horizontalPercent: 3.5,
      verticalPercent: 1,
      color: "#000000",
    },
    pad: { ratioWidth: 4, ratioHeight: 5, color: "#eeeeee" },
    watermark: {
      path: "/logo.png",
      anchor: "topLeft",
      scale: 12,
      spacing: 3,
      opacity: 60,
    },
    exportMasks: true,
    preserveFolders: true,
    destinationType: "customFolder",
    subfolder: "Web",
    outputSharpening: { amount: 120, radius: 0.6, threshold: 2 },
  });
  // Beside the originals the folders are the originals' own.
  expect(
    exportSettingsFromDraft({ ...everything, where: "original" }).settings,
  ).toMatchObject({
    destinationType: "originalFolder",
    preserveFolders: false,
    subfolder: "Web",
  });
  // Every image format carries the colour space.
  for (const format of ["avif", "jxl"]) {
    expect(
      exportSettingsFromDraft({ ...everything, format }).settings.colorSpace,
    ).toBe("adobeRgb");
  }
  // A watermark without an image is no watermark.
  expect(
    exportSettingsFromDraft({ ...everything, watermarkPath: null }).settings
      .watermark,
  ).toBeNull();
});

it("keeps settings it does not know when a recipe is updated", () => {
  const base = {
    ...presetFromDraft(draft, "r1", "Web"),
    lastExportPath: "/print",
  } as ExportPreset;
  const updated = presetFromDraft({ ...draft, quality: 60 }, "r1", "Web", base);
  expect(updated.jpegQuality).toBe(60);
  expect(updated.lastExportPath).toBe("/print");
});

it("refuses subfolders that leave the destination", () => {
  expect(subfolderProblem("")).toBeNull();
  expect(subfolderProblem("Web/small")).toBeNull();
  expect(subfolderProblem("../elsewhere")).not.toBeNull();
  expect(subfolderProblem("Web/../..")).not.toBeNull();
  expect(subfolderProblem("C:\\Exports")).not.toBeNull();
});

it("hides the inherited panel’s last-used entry", () => {
  const recipes = [
    presetFromDraft(draft, "__last_used__", "Last"),
    presetFromDraft(draft, "r1", "Web"),
  ];
  expect(savedRecipes(recipes).map((r) => r.id)).toEqual(["r1"]);
});

it("sharpens for print with the radius of the viewing distance", () => {
  const print = exportSettingsFromDraft({ ...everything, sharpening: "print" })
    .settings.outputSharpening;
  // 80 cm at 240 ppi: one arc minute is 0.233 mm, 2.2 px.
  expect(print?.amount).toBe(120);
  expect(print?.radius).toBeCloseTo(2.2, 1);
  expect(
    exportSettingsFromDraft({ ...everything, sharpening: "off" }).settings
      .outputSharpening,
  ).toBeNull();
  expect(
    draftFromPreset(
      presetFromDraft({ ...everything, sharpening: "print" }, "p", "Print"),
    ),
  ).toMatchObject({
    sharpening: "print",
    printDistanceCm: 80,
    printPpi: 240,
  });
});

it("keeps the box of a dimensions resize", () => {
  const box = {
    ...everything,
    resizeMode: "dimensions" as const,
    resizeValue: 1920,
    resizeValue2: 1080,
  };
  expect(exportSettingsFromDraft(box).settings.resize).toEqual({
    mode: "dimensions",
    value: 1920,
    secondValue: 1080,
    dontEnlarge: false,
  });
  expect(draftFromPreset(presetFromDraft(box, "b", "Box"))).toMatchObject({
    resizeValue: 1920,
    resizeValue2: 1080,
  });
  // Other modes send no second side.
  expect(
    exportSettingsFromDraft(everything).settings.resize,
  ).not.toHaveProperty("secondValue");
});

it("sends a text watermark with its font, colour and shadow", () => {
  const text = {
    ...everything,
    watermarkKind: "text" as const,
    watermarkText: "© Studio",
    watermarkFont: "Avenir Next",
    watermarkColor: "#202020",
    watermarkShadow: false,
  };
  expect(exportSettingsFromDraft(text).settings.watermark).toEqual({
    path: "",
    anchor: "topLeft",
    scale: 12,
    spacing: 3,
    opacity: 60,
    text: {
      text: "© Studio",
      fontFamily: "Avenir Next",
      color: "#202020",
      shadow: false,
    },
  });
  expect(draftFromPreset(presetFromDraft(text, "t", "Text"))).toMatchObject({
    watermarkKind: "text",
    watermarkFont: "Avenir Next",
    watermarkShadow: false,
  });
  // Empty text is no watermark.
  expect(
    exportSettingsFromDraft({ ...text, watermarkText: "  " }).settings
      .watermark,
  ).toBeNull();
});

it("persists counter starts in recipes, including zero, and defaults older recipes to one", () => {
  for (const sequenceStart of [0, 98, 4000]) {
    const preset = presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, sequenceStart }, "counter", "Counter");
    expect(draftFromPreset(preset).sequenceStart).toBe(sequenceStart);
    expect(exportSettingsFromPreset(preset).settings.sequenceStart).toBe(sequenceStart);
  }
  const preset = presetFromDraft(DEFAULT_RECIPE_DRAFT, "old", "Old");
  delete preset.sequenceStart;
  expect(draftFromPreset(preset).sequenceStart).toBe(1);
});

it("returns stable subfolder validation kinds", () => {
  expect(subfolderProblem("../elsewhere")).toBe("outside");
  expect(subfolderProblem("C:\\Exports")).toBe("fullPath");
});

it("treats a name of spaces only as no subfolder, shown and saved trimmed", () => {
  expect(subfolderName(undefined)).toBe("");
  expect(subfolderName("   ")).toBe("");
  expect(subfolderName("\t\n ")).toBe("");
  expect(subfolderName("  Web/small  ")).toBe("Web/small");
  for (const subfolder of ["", " ", "   "]) {
    const preset = presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, subfolder }, "web", "Web");
    expect(preset.subfolder).toBe("");
    expect(draftFromPreset(preset).subfolder).toBe("");
    expect(exportSettingsFromPreset(preset).settings.subfolder).toBe("");
    expect(exportSettingsFromDraft({ ...DEFAULT_RECIPE_DRAFT, subfolder }).settings.subfolder).toBe("");
  }
  // An older saved recipe with spaces only also exports as none.
  const older = { ...presetFromDraft(DEFAULT_RECIPE_DRAFT, "web", "Web"), subfolder: "   " };
  expect(exportSettingsFromPreset(older).settings.subfolder).toBe("");
  expect(presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, subfolder: " Web " }, "web", "Web").subfolder).toBe("Web");
});

it("keeps explicit contact retention through recipes and export, with omission as the old default", () => {
  const preset = presetFromDraft({ ...draft, keepCreatorContact: true }, "r1", "Anonymous contacts");
  expect(draftFromPreset(preset).keepCreatorContact).toBe(true);
  expect(exportSettingsFromPreset(preset).settings.keepCreatorContact).toBe(true);
  delete preset.keepCreatorContact;
  expect(exportSettingsFromPreset(preset).settings.keepCreatorContact).toBe(false);
});

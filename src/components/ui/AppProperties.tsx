import type { ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import { ExportPreset } from './ExportImportProperties';
import { Adjustments, CopyPasteSettings } from '../../utils/adjustments';
import { ToolType } from '../panel/right/Masks';
import type { ModuleLayout, WorkspaceModule } from '../../utils/panelLayout';
import type { ImportMetadataPreset } from '../session/IngestMetadata';
import type { MetadataSelection } from '../../utils/librarySearch';

export const GLOBAL_KEYS = [
  ' ',
  'ArrowUp',
  'ArrowDown',
  'ArrowLeft',
  'ArrowRight',
  'f',
  'b',
  'a',
  's',
  'd',
  'r',
  'm',
  'k',
  'p',
  'i',
  'e',
  '0',
  '1',
  '2',
  '3',
  '4',
  '5',
  'Enter',
];
export const OPTION_SEPARATOR = 'separator';
/** A small group caption in a menu (`label`), not selectable. */
export const OPTION_HEADER = 'header';

export enum Invokes {
  DeleteFilesWithAssociated = 'delete_files_with_associated',
  DeleteFilesFromDisk = 'delete_files_from_disk',
  PreviewNegativeConversion = 'preview_negative_conversion',
  ConvertNegatives = 'convert_negatives',
  ClearImageCaches = 'clear_image_caches',
  CalculateGuidedPerspective = 'calculate_guided_perspective',
  SessionIngestPreview = 'session_ingest_preview',
  SessionIngestCaptureInfo = 'session_ingest_capture_info',
  SessionIngestThumbnail = 'session_ingest_thumbnail',
  LibraryCopy = 'library_copy',
  SessionCancelIngest = 'session_cancel_ingest',
  SessionLastDirectory = 'session_last_directory',
  SessionLast = 'session_last',
  SessionNamingPreview = 'session_naming_preview',
  SessionFlags = 'session_flags',
  HotFolderSuspend = 'hot_folder_suspend',
  HotFolderResume = 'hot_folder_resume',
  HotFolderStart = 'hot_folder_start',
  HotFolderStop = 'hot_folder_stop',
  LibraryImportPreview = 'library_import_preview',
  LibraryImport = 'library_import',
  SessionSummary = 'session_summary',
  SessionSetFlag = 'session_set_flag',
  SessionOpen = 'session_open',
  SessionCreate = 'session_create',
  SessionIngestScan = 'session_ingest_scan',
  SessionIngest = 'session_ingest',
  SessionIngestJobs = 'session_ingest_jobs',
  SessionIngestResume = 'session_ingest_resume',
  SessionIngestForget = 'session_ingest_forget',
  ListLuts = 'list_luts',
  ImportLuts = 'import_luts',
  RemoveLut = 'remove_lut',
  GenerateAiDepthMask = 'generate_ai_depth_mask',
  FrontendReady = 'frontend_ready',
  CancelThumbnailGeneration = 'cancel_thumbnail_generation',
  LoadAndParseLut = 'load_and_parse_lut',
  ApplyAutoLensCorrectionToPaths = 'apply_auto_lens_correction_to_paths',
  ResolvePastedLens = 'resolve_pasted_lens',
  UpdateThumbnailQueue = 'update_thumbnail_queue',
  BeginImageListing = 'begin_image_listing',
  NextImageListingPage = 'next_image_listing_page',
  CancelImageListing = 'cancel_image_listing',
  AddTagForPaths = 'add_tag_for_paths',
  ApplyAdjustments = 'apply_adjustments',
  ApplyAdjustmentsToPaths = 'apply_adjustments_to_paths',
  ApplyAutoAdjustmentsToPaths = 'apply_auto_adjustments_to_paths',
  ApplyAiDenoise = 'apply_ai_denoise',
  ThirdPartyLicenses = 'third_party_licenses',
  CalculateAutoAdjustments = 'calculate_auto_adjustments',
  CalculateManualTransform = 'calculate_manual_transform',
  ListFontFamilies = 'list_font_families',
  CancelAiTask = 'cancel_ai_task',
  CancelExport = 'cancel_export',
  ClearAllSidecars = 'clear_all_sidecars',
  ClearAiTags = 'clear_ai_tags',
  ClearAllTags = 'clear_all_tags',
  ClearThumbnailCache = 'clear_thumbnail_cache',
  CopyFiles = 'copy_files',
  CreateFolder = 'create_folder',
  CreateVirtualCopy = 'create_virtual_copy',
  CullImages = 'cull_images',
  DeleteFolder = 'delete_folder',
  DuplicateFile = 'duplicate_file',
  EstimateExportSizes = 'estimate_export_sizes',
  ExportImages = 'export_images',
  ExportImagesBatch = 'export_images_batch',
  ExportQueueJobs = 'export_queue_jobs',
  ExportQueueResume = 'export_queue_resume',
  ExportQueueRemove = 'export_queue_remove',
  ExportQueueItems = 'export_queue_items',
  ExportQueuePause = 'export_queue_pause',
  ExportQueueCancel = 'export_queue_cancel',
  ExportQueueHold = 'export_queue_hold',
  FrontendLog = 'frontend_log',
  GenerateRelightDepth = 'generate_relight_depth',
  LoadRelightPattern = 'load_relight_pattern',
  GenerateAiForegroundMask = 'generate_ai_foreground_mask',
  GenerateAiSkyMask = 'generate_ai_sky_mask',
  GenerateAiPeopleMask = 'generate_ai_people_mask',
  DetectPeople = 'detect_people',
  AnalyzeFaceRetouch = 'analyze_face_retouch',
  ApplyFaceRetouchToPaths = 'apply_face_retouch_to_paths',
  GenerateAiSubjectMask = 'generate_ai_subject_mask',
  PrecomputeAiSubjectMask = 'precompute_ai_subject_mask',
  PrecomputeAiForegroundMask = 'precompute_ai_foreground_mask',
  PrecomputeAiSkyMask = 'precompute_ai_sky_mask',
  PreloadAiMaskModels = 'preload_ai_mask_models',
  WarmUpAiMaskModels = 'warm_up_ai_mask_models',
  UnloadAiModels = 'unload_ai_models',
  NoteAiMaskUse = 'note_ai_mask_use',
  GeneratePreviewForPath = 'generate_preview_for_path',
  GenerateMaskOverlay = 'generate_mask_overlay',
  GenerateMaskThumbnails = 'generate_mask_thumbnails',
  GenerateUncroppedPreview = 'generate_uncropped_preview',
  GetFolderTree = 'get_folder_tree',
  GetFolderChildren = 'get_folder_children',
  GetLogFilePath = 'get_log_file_path',
  GetPinnedFolderTrees = 'get_pinned_folder_trees',
  GetSupportedFileTypes = 'get_supported_file_types',
  HandleExportPresetsToFile = 'handle_export_presets_to_file',
  HandleImportPresetsFromFile = 'handle_import_presets_from_file',
  HandleImportPresetsFromFiles = 'handle_import_presets_from_files',
  HandleImportLegacyPresetsFromFile = 'handle_import_legacy_presets_from_file',
  InvokeGenerativeReplaseWithMaskDef = 'invoke_generative_replace_with_mask_def',
  AutodetectLens = 'autodetect_lens',
  CalculateUpright = 'calculate_upright',
  CalculateAutoStraighten = 'calculate_auto_straighten',
  GetCameraLensProfile = 'get_camera_lens_profile',
  GetLensDistortionParams = 'get_lens_distortion_params',
  GetLensfunLensesForMaker = 'get_lensfun_lenses_for_maker',
  GetLensfunMakers = 'get_lensfun_makers',
  ImportCameraProfile = 'import_camera_profile',
  ListCameraProfiles = 'list_camera_profiles',
  WhiteBalanceInfo = 'white_balance_info',
  WhiteBalanceFromSample = 'white_balance_from_sample',
  AutoGrayMix = 'auto_gray_mix',
  AutoWhiteBalance = 'auto_white_balance_for',
  PlaceLightroomMasks = 'place_lightroom_masks',
  PlaceLightroomRetouch = 'place_lightroom_retouch',
  DetectRedEye = 'detect_red_eye',
  RedEyePoint = 'red_eye_point',
  RedEyesShown = 'red_eyes_shown',
  FindSpotSource = 'find_spot_source',
  VisualizeSpots = 'visualize_spots',
  LoadImage = 'load_image',
  LoadImageProxy = 'load_image_proxy',
  ActivatePhoto = 'activate_photo',
  PreparePreviewProxies = 'prepare_preview_proxies',
  PrewarmMaskSource = 'prewarm_mask_source',
  SetMultiView = 'set_multi_view',
  RenderBrowserView = 'render_browser_view',
  SetMultiViewZoom = 'set_multi_view_zoom',
  SetMultiViewLive = 'set_multi_view_live',
  BuildPreviews = 'build_previews',
  StopPreviewBuild = 'stop_preview_build',
  PreviewBuildProgress = 'preview_build_progress',
  ProblemReportContext = 'problem_report_context',
  ProblemReportScreenshot = 'problem_report_screenshot',
  DiscardPreviews = 'discard_previews',
  PreviewCacheUsage = 'preview_cache_usage',
  ClearPreviewCache = 'clear_preview_cache',
  ApplyPreviewLimits = 'apply_preview_limits',
  LoadEmbeddedPreview = 'load_embedded_preview',
  LoadMetadata = 'load_metadata',
  LoadPresets = 'load_presets',
  LoadBuiltinPresets = 'load_builtin_presets',
  CheckUpdates = 'check_updates',
  ContributionAppInfo = 'contribution_app_info',
  ContributionAvailable = 'contribution_available',
  ContributionDelete = 'contribution_delete',
  ContributionPrepare = 'contribution_prepare',
  ContributionSend = 'contribution_send',
  CrashReportMarkAsked = 'crash_report_mark_asked',
  CrashReportRemove = 'crash_report_remove',
  CrashReportsPending = 'crash_reports_pending',
  CrashReportsSetEnabled = 'crash_reports_set_enabled',
  UsageStatsNoteTools = 'usage_stats_note_tools',
  UsageStatsPreview = 'usage_stats_preview',
  UsageStatsSetEnabled = 'usage_stats_set_enabled',
  LoadSettings = 'load_settings',
  MoveFiles = 'move_files',
  MoveFolder = 'move_folder',
  PlanMoveFiles = 'plan_move_files',
  PlanMoveFolder = 'plan_move_folder',
  ReadExifForPaths = 'read_exif_for_paths',
  RemoveTagForPaths = 'remove_tag_for_paths',
  RenameFiles = 'rename_files',
  RenameFolder = 'rename_folder',
  UndoFileOperation = 'undo_file_operation',
  RedoFileOperation = 'redo_file_operation',
  StopFileOperation = 'stop_file_operation',
  TrashKeptCopy = 'trash_kept_copy',
  SessionTrashContents = 'session_trash_contents',
  EmptySessionTrash = 'empty_session_trash',
  ResetAdjustmentsForPaths = 'reset_adjustments_for_paths',
  SaveMetadataAndUpdateThumbnail = 'save_metadata_and_update_thumbnail',
  SaveCollage = 'save_collage',
  SnapshotsList = 'snapshots_list',
  SnapshotCreate = 'snapshot_create',
  SnapshotRename = 'snapshot_rename',
  SnapshotUpdate = 'snapshot_update',
  SnapshotDelete = 'snapshot_delete',
  SnapshotThumbnail = 'snapshot_thumbnail',
  SessionAlbumAdd = 'session_album_add',
  SessionAllTextMetadata = 'session_all_text_metadata',
  SessionAlbumPaths = 'session_album_paths',
  SessionAlbumRemove = 'session_album_remove',
  SessionAlbums = 'session_albums',
  SessionCreateAlbum = 'session_create_album',
  SessionDeleteAlbum = 'session_delete_album',
  SessionMoveAlbum = 'session_move_album',
  SessionOffline = 'session_offline',
  SessionRelink = 'session_relink',
  SessionRelinkFolder = 'session_relink_folder',
  SessionPreviousImport = 'session_previous_import',
  FolderSyncScan = 'folder_sync_scan',
  FolderSyncCancel = 'folder_sync_cancel',
  FolderSyncApply = 'folder_sync_apply',
  CatalogFolders = 'catalog_folders',
  SessionRenameAlbum = 'session_rename_album',
  SessionSearch = 'session_search',
  SessionPathsAsListed = 'session_paths_as_listed',
  SessionFacets = 'session_facets',
  SessionCreateSmartAlbum = 'session_create_smart_album',
  SessionSetSmartAlbumRules = 'session_set_smart_album_rules',
  SessionSetTextMetadata = 'session_set_text_metadata',
  SavePresets = 'save_presets',
  SaveSettings = 'save_settings',
  SampleLumaRange = 'sample_luma_range',
  SamplePreviewColor = 'sample_preview_color',
  SampleCurveInput = 'sample_curve_input',
  SamplePreviewWhiteBalance = 'sample_preview_white_balance',
  SamplePreviewGreyWorld = 'sample_preview_grey_world',
  SetColorLabelForPaths = 'set_color_label_for_paths',
  SetRatingForPaths = 'set_rating_for_paths',
  RestoreCulling = 'restore_culling',
  ShowInFinder = 'show_in_finder',
  SetAppMenu = 'set_app_menu',
  SetAppMenuState = 'set_app_menu_state',
  AppMenuTextAction = 'app_menu_text_action',
  StartBackgroundIndexing = 'start_background_indexing',
  MergePhotos = 'merge_photos',
  SaveMerge = 'save_merge',
  RefinishMerge = 'refinish_merge',
  DiscardMerge = 'discard_merge',
  QuitSaving = 'quit_saving',
  FinishQuit = 'finish_quit',
  CancelQuit = 'cancel_quit',
  UpdateWgpuTransform = 'update_wgpu_transform',
  RenderSplitBefore = 'render_split_before',
  UpdateExifFields = 'update_exif_fields',
  FileSize = 'file_size',
  GetAlbums = 'get_albums',
  SaveAlbums = 'save_albums',
  AddToAlbum = 'add_to_album',
  GetAlbumImages = 'get_album_images',
  TetherAutofocus = 'tether_autofocus',
  TetherCameras = 'tether_cameras',
  TetherCapture = 'tether_capture',
  TetherConnect = 'tether_connect',
  TetherDisconnect = 'tether_disconnect',
  TetherLiveView = 'tether_live_view',
  TetherLiveViewFrame = 'tether_live_view_frame',
  TetherNextCapture = 'tether_next_capture',
  TetherSetSetting = 'tether_set_setting',
  TetherSettings = 'tether_settings',
  TetherStatus = 'tether_status',
}

export enum ExifOverlay {
  Off = 'off',
  Hover = 'hover',
  Always = 'always',
}

export enum Panel {
  Adjustments = 'adjustments',
  Crop = 'crop',
  Export = 'export',
  Masks = 'masks',
  Metadata = 'metadata',
  Presets = 'presets',
  FolderTree = 'folderTree',
}

export type PanelRegion = 'leftTop' | 'leftBottom' | 'rightTop' | 'rightBottom';

export enum RawStatus {
  All = 'all',
  NonRawOnly = 'nonRawOnly',
  RawOnly = 'rawOnly',
}

export enum SortDirection {
  Ascending = 'asc',
  Descending = 'desc',
}

type FolderSortKey = 'name' | 'modified' | 'created' | 'imageCount';

export interface FolderTreeSort {
  key: FolderSortKey;
  order: SortDirection;
}

export enum ThumbnailAspectRatio {
  Cover = 'cover',
  Contain = 'contain',
  Justified = 'justified',
}

export interface WorkspaceState {
  leftPanelWidth: number;
  rightPanelWidth: number;
  leftTopHeight: number;
  rightTopHeight: number;
  panelLayout: Record<PanelRegion, Panel[]>;
  activePanels: Record<PanelRegion, Panel | null>;
  panelSwitcherPlacement: Record<PanelRegion, 'left' | 'right' | 'top' | 'bottom'>;
}

export interface CustomAspectRatio {
  width: number;
  height: number;
}

export type GroupPreference = 'jpeg' | 'raw';
export type GroupingMode = 'off' | GroupPreference;

/** Capture One-style mask visibility in the editor viewer. */
export type MaskDisplay = 'always' | 'drawing' | 'grayscale' | 'never';

/** Library location restored at startup (`LastFolderState` in app_settings.rs). */
export interface LastFolderState {
  currentFolderPath: string | null;
  expandedFolders: string[];
  activeAlbumId: string | null;
  expandedAlbumGroups: string[];
}

/** A lens saved in the settings for manual lens correction (`MyLens` in app_settings.rs). */
export interface MyLens {
  maker: string;
  model: string;
}

/** What `activate_photo` put on screen natively for the next photograph. */
export type Activation = 'none' | 'developed' | 'still';

/** Crop grid: always, only while the frame is dragged (Capture One), or off. */
export type CropGrid = 'always' | 'drag' | 'off';

export interface AppSettings {
  checkForUpdates?: boolean;
  /** Rawtonic's own presets and styles in the Styles panel and the tools' preset menus (missing: shown). */
  showBuiltinPresets?: boolean;
  /** Develop: a rating or flag key steps to the next photograph (missing: off). */
  autoAdvance?: boolean;
  /** Offer a crash report after Rawtonic quit unexpectedly (missing: yes). */
  crashReports?: boolean;
  /** Anonymous weekly usage statistics (ADR 0017); off unless true. */
  usageStats?: boolean;
  updatePrereleases?: boolean;
  dismissedUpdateVersion?: string;
  aiConnectorAddress?: string;
  aiProvider?: string;
  decorations?: boolean;
  editorPreviewResolution?: number;
  smallThumbnailResolution?: number;
  mediumThumbnailResolution?: number;
  enableZoomHifi?: boolean;
  useFullDpiRendering?: boolean;
  highResZoomMultiplier?: number;
  enableLivePreviews?: boolean;
  livePreviewQuality?: string;
  maskDisplay?: MaskDisplay;
  maskDisplayRestore?: MaskDisplay;
  maskColor?: string;
  /** Size of the interface's panels and bars (1, 1.12 or 1.25). */
  uiScale?: number;
  maskOpacity?: number;
  radialDrawInside?: boolean;
  cropGrid?: CropGrid;
  cropShade?: number;
  /** Open tools ("<column>/<tool>") and the tool order per column. */
  toolLayout?: ToolLayout;
  /** The brush's last size, hardness, opacity and flow. */
  brushDefaults?: Partial<Pick<BrushSettings, "size" | "hardness" | "opacity" | "flow">>;
  enableAiTagging?: boolean;
  aiTagCount?: number;
  customAiTags?: string[];
  filterCriteria?: FilterCriteria;
  lastFolderState?: LastFolderState | null;
  pinnedFolders?: string[];
  lastRootPath: string | null;
  rootFolders?: string[];
  recentLibraries?: string[];
  libraryViewMode?: LibraryViewMode;
  sortCriteria?: SortCriteria;
  /** Legacy: there is no theme choice any more; old values are kept but ignored. */
  theme?: string;
  thumbnailSize?: ThumbnailSize;
  thumbnailAspectRatio?: ThumbnailAspectRatio;
  uiVisibility?: UiVisibility;
  rawHighlightCompression?: number;
  processingBackend?: string;
  aiGpu?: boolean;
  linuxGpuOptimization?: boolean;
  exportPresets?: ExportPreset[];
  importMetadataPresets?: ImportMetadataPreset[];
  myLenses?: MyLens[];
  customAspectRatios?: CustomAspectRatio[];
  adjustmentLayout?: AdjustmentLayout;
  enableFolderImageCounts?: boolean;
  displayEditIcon?: boolean;
  linearRawMode?: string;
  thumbnailWorkerThreads?: number;
  imageCacheSize?: number;
  rawPreprocessingColorNr?: number;
  rawPreprocessingSharpening?: number;
  applyPreprocessingToNonRaws?: boolean;
  useAppleRaw9?: boolean;
  enableXmpSync?: boolean;
  createXmpIfMissing?: boolean;
  isWaveformVisible?: boolean;
  waveformHeight?: number;
  activeWaveformChannel?: string;
  useWgpuRenderer?: boolean;
  /** Replaced by viewerBackground (true → 'medium'); still read from old settings. */
  editorNeutralGreyBg?: boolean;
  /** Develop viewer background around the photograph (utils/viewerDisplay). */
  viewerBackground?: string;
  /** Develop viewer: fit leaves a proof margin around the photograph (X). */
  viewerProofMargin?: boolean;
  /** Proof margin size in % of the viewer's shorter side. */
  viewerProofMarginPercent?: number;
  canvasInputMode?: 'mouse' | 'trackpad';
  zoomSpeedMultiplier?: number;
  zoomPhotoToPixelClick?: boolean;
  keybinds?: { [action: string]: string[] };
  tonemapperOverrideEnabled?: boolean;
  defaultRawTonemapper?: string;
  defaultNonRawTonemapper?: string;
  copyPasteSettings?: CopyPasteSettings;
  enableFocusMode?: boolean;
  enableToolFocusMode?: boolean;
  openTreeSections?: string[];
  folderIcons?: Record<string, string>;
  exifOverlay?: ExifOverlay;
  language?: string;
  fontFamily?: string;
  folderTreeSort?: FolderTreeSort;
  taggingShortcuts?: string[];
  libraryDisplayMode?: LibraryDisplayMode;
  grouping?: GroupingMode;
  requireMatchingExif?: boolean;
  groupEditedFiles?: boolean;
  groupPreferredType?: GroupPreference; // legacy
  alwaysDecodeRawThumbnails?: boolean;
  /** Long edge of Standard previews at least (px); unset or 0: sized to the Develop view. */
  standardPreviewSize?: number;
  /** Days after their last use when 1:1 previews are discarded; unset: 30, 0: never. */
  discardFullPreviewsDays?: number;
  /** Most the Standard previews of one previews folder may take (GB); unset: 10, 0: no limit. */
  standardPreviewsLimitGb?: number;
  workspace?: WorkspaceState;
}

export interface BrushSettings {
  feather: number;
  /** Capture One's brush opacity and flow in percent (default 100). */
  flow?: number;
  /** Capture One's brush hardness in percent; replaces feather when set. */
  hardness?: number;
  opacity?: number;
  size: number;
  tool: ToolType;
  /** Lightroom's and Capture One's Auto Mask: strokes keep to colours like
   * the one under the brush centre. */
  autoMask?: boolean;
  /** Pen pressure scales new pen strokes' size and/or flow (absent: size on,
   * flow off, as Capture One's Pen Pressure). A mouse paints at full
   * pressure (user decision D3). */
  pressureSize?: boolean;
  pressureFlow?: boolean;
}

export enum LibraryViewMode {
  Flat = 'flat',
  Recursive = 'recursive',
}

export const EditedStatus = {
  All: 'all',
  EditedOnly: 'editedOnly',
  UneditedOnly: 'uneditedOnly',
} as const;

export type EditedStatus = (typeof EditedStatus)[keyof typeof EditedStatus];

export interface FilterCriteria {
  colors: Array<string>;
  rating: number;
  rawStatus: RawStatus;
  editedStatus?: EditedStatus;
  flag?: 'all' | 'picked' | 'unflagged' | 'rejected';
  /** Values chosen in the Library's metadata lists (camera, lens, …); the
   * library's catalog finds the photographs that have them. */
  metadata?: MetadataSelection;
}

export interface Folder {
  /** Created by a tool preset menu; hide while empty. Explicit user folders stay. */
  automatic?: boolean;
  children: Preset[];
  id?: string | undefined;
  name?: string | undefined;
  imageCount?: number;
}

export interface ImageFile {
  is_edited: boolean;
  modified: number;
  path: string;
  rating: number;
  tags: Array<string> | null;
  exif: { [key: string]: string } | null;
  is_virtual_copy: boolean;
  is_cloud_placeholder: boolean;
  is_raw: boolean;
  group_id: string | null;
  /** The file is missing or on a disconnected drive; the library still holds
   * the photograph (it opens on its previews). */
  is_offline?: boolean;
  /** The disconnected drive the file is on. */
  offline_volume?: string | null;
}

/** A photograph whose file is offline, as the session lists it. */
export interface OfflineEntry {
  path: string;
  volume: string | null;
  mtime: number;
  /** Its folder is gone too (renamed, moved, or its drive disconnected). */
  folder_missing: boolean;
  /** Its virtual copies the library keeps (copy ids), offline with it. */
  copies?: string[];
}

/** What a relink found and did (`session_relink`, `session_relink_folder`). */
export interface RelinkReport {
  relinked: Array<[string, string]>;
  mismatched: string[];
  taken: string[];
  not_found: number;
}

/** Props the context menu passes to an option's `customComponent`, next to the option's `customProps`. */
export interface ContextMenuCustomProps {
  hideContextMenu(): void;
}

export interface ToolLayout {
  open?: Record<string, boolean>;
  order?: Record<string, string[]>;
}

export interface Option {
  color?: string;
  /**
   * Replaces a submenu's rows when it is that submenu's only entry, rendered as
   * `<customComponent {...customProps} hideContextMenu={…} />`. A method signature,
   * so components taking no props or extending ContextMenuCustomProps both fit.
   */
  customComponent?(props: ContextMenuCustomProps): ReactNode;
  customProps?: object;
  /** A choice in a group: shows a check mark when true, room for one when false. */
  checked?: boolean;
  disabled?: boolean;
  icon?: LucideIcon;
  isDestructive?: boolean;
  label?: string;
  onClick?(): void;
  onRightClick?(): void;
  submenu?: Option[];
  type?: string;
}

export enum Orientation {
  Horizontal = 'horizontal',
  Vertical = 'vertical',
}

export interface Preset {
  adjustments: Partial<Adjustments>;
  folder?: Folder;
  id: string;
  name: string;
  includeMasks?: boolean;
  includeCropTransform?: boolean;
  presetType?: 'tool' | 'style';
  /** A preset of one tool's menu: that tool's parameters, which "Update
   * with current settings" keeps to. */
  toolKeys?: string[];
  /** One of Rawtonic's own, read-only presets (`load_builtin_presets`). */
  builtin?: boolean;
  /** A built-in's revision; recipes keep the values it had when applied. */
  version?: number;
}

export interface Progress {
  stage?: string;
  completed?: number;
  current?: number;
  total: number;
}

/** Sidecar metadata from `load_metadata` / `load_image` (`ImageMetadata` in image_processing.rs). */
export interface ImageMetadata {
  version: number;
  rating: number;
  /** The stored recipe as saved; `null` when the image has never been edited. */
  adjustments: Adjustments | null;
  tags: string[] | null;
  exif?: { [key: string]: string };
}

/** Result of `load_image` (`LoadImageResult` in image_loader.rs). */
export interface LoadImageResult {
  width: number;
  height: number;
  metadata: ImageMetadata;
  exif: { [key: string]: string };
  is_raw: boolean;
  /** AI Denoise amount the source was developed with (0 … 100). */
  ai_denoise?: number;
  /** Its `aiDenoiseVersion` (1 when off). */
  ai_denoise_version?: number;
  /** Why the recipe's AI Denoise could not be applied. */
  ai_denoise_error?: string | null;
  /** The RAW is offline; the photograph opened on its 1:1 or Standard preview. */
  offline?: OfflinePreview | null;
  /** The source was developed for the multi-scale colour noise filter (no
   * decode-time chroma clean-up); null for preview data. */
  color_noise_multiscale?: boolean | null;
  /** The source was developed with the second dead-photosite test
   * (`renderVersion` 3); null for preview data. */
  dead_pixel_adjacent?: boolean | null;
  /** The RAW data could not be developed; the camera's embedded preview is
   * shown. */
  developed_from_preview?: boolean;
  /** ... and a camera sample could help (not for a format no open decoder
   * reads): Develop offers camera help. */
  camera_help?: boolean;
}

/** Which preview an offline photograph shows: its 1:1 preview (full
 * resolution) or its Standard preview (about screen size). */
export type OfflinePreview = 'full' | 'standard';

/** A preview build (`BuildProgress` in proxy_worker.rs). */
export interface PreviewBuildProgress {
  done: number;
  total: number;
  running: boolean;
  stopped: boolean;
  skipped: number;
  failedCount: number;
  failed: Array<{ path: string; reason: string }>;
}

/** Disk use of a previews folder (`PreviewUsage` in preview_proxy.rs). */
export interface PreviewUsage {
  standardCount: number;
  standardBytes: number;
  fullCount: number;
  fullBytes: number;
}

/** `PreviewCacheInfo` in proxy_worker.rs. */
export interface PreviewCacheInfo {
  session: { dir: string; usage: PreviewUsage } | null;
  outside: { dir: string; usage: PreviewUsage } | null;
}

export interface SelectedImage {
  exif: { [key: string]: string } | null;
  group_id?: string | null;
  height: number;
  isRaw: boolean;
  /** Source and recipe are in: the photograph is shown and can be edited
   * (from its preview proxy before the full decode, as in Capture One). */
  isReady: boolean;
  /** A preview proxy is the native source until the full decode. */
  isPreviewReady?: boolean;
  /** The full RAW decode is in (full-resolution rendering; commands that
   * need it wait for it natively). */
  isDecoded?: boolean;
  /** AI Denoise amount the decoded source carries (0 … 100). */
  aiDenoise?: number;
  /** Its `aiDenoiseVersion` (missing: 1). */
  aiDenoiseVersion?: number;
  /** An AI Denoise amount that failed for this source (not retried). */
  aiDenoiseFailed?: number;
  /** The `aiDenoiseVersion` that failed with it (missing: 1). */
  aiDenoiseFailedVersion?: number;
  /** The decoded source was developed for the multi-scale colour noise
   * filter (`colorNoiseMultiscale` in src/utils/adjustments.ts); unknown for
   * preview data. */
  colorNoiseMultiscale?: boolean;
  /** A colour noise filter the source failed to develop for (not retried). */
  colorNoiseMultiscaleFailed?: boolean;
  /** The decoded source was developed with the second dead-photosite test
   * (`deadPixelAdjacent` in src/utils/adjustments.ts); unknown for preview
   * data. */
  deadPixelAdjacent?: boolean;
  /** A dead-photosite test the source failed to develop for (not retried). */
  deadPixelAdjacentFailed?: boolean;
  /** The RAW is offline: the photograph shows (and is edited on) this preview. */
  offline?: OfflinePreview | null;
  metadata?: ImageMetadata | null;
  original_base64?: string;
  path: string;
  thumbnailUrl: string;
  thumbnailSource?: 'embedded';
  width: number;
}

/** Whether the viewer can render the photograph (proxy or full decode). */
export const canRenderSource = (image: SelectedImage | null | undefined): image is SelectedImage =>
  Boolean(image?.isReady || image?.isPreviewReady);

export interface SortCriteria {
  key: string;
  label?: string;
  order: string;
}

export interface SupportedTypes {
  nonRaw: Array<string>;
  raw: Array<string>;
}

export enum LibraryDisplayMode {
  Grid = 'grid',
  Cull = 'cull',
  List = 'list',
}

export enum ThumbnailSize {
  Large = 'large',
  Medium = 'medium',
  Small = 'small',
}

export interface TransformState {
  positionX: number;
  positionY: number;
  scale: number;
}

export interface UiVisibility {
  filmstrip: boolean;
  leftPanel: boolean;
  rightPanel: boolean;
  quickFilter?: boolean;
  /** The Studio workspace's layout per module (`src/utils/panelLayout.ts`). */
  modules?: Partial<Record<WorkspaceModule, Partial<ModuleLayout>>>;
}

export interface CollapsibleSectionsState {
  basic: boolean;
  color: boolean;
  curves: boolean;
  details: boolean;
  effects: boolean;
}

export interface AdjustmentLayout {
  collapsedTools?: string[];
  hiddenSections?: string[];
  hiddenTools?: string[];
  openSections?: Partial<CollapsibleSectionsState>;
  sectionOrder?: string[];
  toolOrder?: Record<string, string[]>;
}

/** Scope images from `analytics-update` (`WaveformData` in image_processing.rs). */
export interface WaveformData {
  height: number;
  luma: string;
  rgb: string;
  parade: string;
  vectorscope: string;
  width: number;
}

/** Per-channel 256-bin histogram of the rendered preview (`HistogramData` in image_processing.rs). */
export interface HistogramData {
  blue: Array<number>;
  green: Array<number>;
  luma: Array<number>;
  red: Array<number>;
}

export interface CullingSettings {
  similarityThreshold: number;
  blurThreshold: number;
  groupSimilar: boolean;
  filterBlurry: boolean;
}

interface ImageAnalysisResult {
  path: string;
  qualityScore: number;
  sharpnessMetric: number;
  centerFocusMetric: number;
  exposureMetric: number;
  width: number;
  height: number;
}

interface CullGroup {
  representative: ImageAnalysisResult;
  duplicates: ImageAnalysisResult[];
}

export interface CullingSuggestions {
  similarGroups: CullGroup[];
  blurryImages: ImageAnalysisResult[];
  failedPaths: string[];
}

export type AlbumItem = Album | AlbumGroup;

export interface Album {
  type: 'album';
  id: string;
  name: string;
  icon?: string;
  images: string[];
}

export interface AlbumGroup {
  type: 'group';
  id: string;
  name: string;
  icon?: string;
  children: AlbumItem[];
}

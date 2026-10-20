import { useMemo } from 'react';
import { useLibraryExif } from './useLibraryExif';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import {
  RawStatus, EditedStatus, SortDirection, ImageFile, GroupingMode, type SortCriteria,
} from '../components/ui/AppProperties';
import { buildImageGroups, GroupBadgeInfo, GroupId } from '../utils/imageGrouping';
import { flagName, imageFlag, type FlagCounts } from '../utils/culling';
import { photographPath } from '../utils/librarySearch';
import { performanceDiagnosticsEnabled, reportPerformance } from './usePerformanceDiagnostics';

// Extract each basename once per immutable file record, not twice per comparison.
// Weak keys release old folders; the locale comparison keeps the existing order.
const filenames = new WeakMap<ImageFile, string>();
const nameCollator = new Intl.Collator();
function filename(image: ImageFile): string {
  let name = filenames.get(image);
  if (name === undefined) {
    name = image.path.slice(Math.max(image.path.lastIndexOf('/'), image.path.lastIndexOf('\\')) + 1);
    filenames.set(image, name);
  }
  return name;
}

export const ADVANCED_QUERY_REGEX =
  /^(iso|aperture|f|shutter|s|focal|mm|rating|color|camera|make|model|lens)\s*(?::)?\s*(>=|<=|>|<|=)?\s*(.+)$/i;

const parseShutter = (val: string | undefined): number => {
  if (!val) return 0;
  const cleanVal = val.replace(/s/i, '').trim();
  const parts = cleanVal.split('/');
  if (parts.length === 2) {
    const num = parseFloat(parts[0]);
    const den = parseFloat(parts[1]);
    return den !== 0 ? num / den : 0;
  }
  const numVal = parseFloat(cleanVal);
  return isNaN(numVal) ? 0 : numVal;
};

const parseAperture = (val: string | undefined): number => {
  if (!val) return 0;
  const match = val.match(/(\d+(\.\d+)?)/);
  const numVal = match ? parseFloat(match[0]) : 0;
  return isNaN(numVal) ? 0 : numVal;
};

const parseFocalLength = (val: string | undefined): number => {
  if (!val) return 0;
  const match = val.match(/(\d+(\.\d+)?)/);
  if (!match) return 0;
  const numVal = parseFloat(match[0]);
  return isNaN(numVal) ? 0 : numVal;
};

export interface GroupedLibrary {
  displayList: ImageFile[];
  badges: Map<GroupId, GroupBadgeInfo> | null;
  flagCounts: FlagCounts;
}

export type LibraryQueryState = Pick<ReturnType<typeof useLibraryStore.getState>,
  'imageList' | 'imageRatings' | 'filterCriteria' | 'searchCriteria' | 'sortCriteria'> & {
    imageFlags?: Record<string, number>;
    catalogMatches?: Set<string> | null;
  };
interface QuerySettings {
  appSettings?: { grouping?: GroupingMode; groupEditedFiles?: boolean } | null;
}

// Filtering/flagging does not change filename order. Retain one ordering for each
// immutable source list, with weak keys so leaving a folder releases its records.
const orderings = new WeakMap<ImageFile[], {
    key: SortCriteria['key'];
    order: SortCriteria['order'];
    ratings: Record<string, number> | null;
    files: ImageFile[];
  }>();

function cachedOrdering(
  images: ImageFile[], imageRatings: Record<string, number>, sort: SortCriteria,
): ImageFile[] | undefined {
  const cached = orderings.get(images);
  const ratings = sort.key === 'rating' ? imageRatings : null;
  return cached && cached.key === sort.key && cached.order === sort.order && cached.ratings === ratings
    ? cached.files : undefined;
}

function orderedImages(images: ImageFile[], imageRatings: Record<string, number>, sort: SortCriteria): ImageFile[] {
  const { key, order } = sort;
  const ratings = key === 'rating' ? imageRatings : null;
  const cached = cachedOrdering(images, imageRatings, sort);
  if (cached) return cached;
  const isNameSort = ![
    'date_taken', 'iso', 'shutter_speed', 'aperture', 'focal_length', 'date', 'rating', 'edited',
  ].includes(key);

  // Prepare numeric and filename keys once, rather than parsing EXIF or looking
  // up a WeakMap for both images on every comparison in the O(n log n) sort.
  const keyed = images.map((image) => {
    let value = 0;
    switch (key) {
      case 'iso':
        value = parseInt(image.exif?.PhotographicSensitivity || image.exif?.ISOSpeedRatings || '0', 10) || 0;
        break;
      case 'shutter_speed':
        value = parseShutter(image.exif?.ExposureTime);
        break;
      case 'aperture':
        value = parseAperture(image.exif?.FNumber);
        break;
      case 'focal_length':
        value = parseFocalLength(image.exif?.FocalLength);
        break;
      case 'date':
        value = image.modified;
        break;
      case 'rating':
        value = imageRatings[image.path] ?? image.rating;
        break;
      case 'edited':
        value = image.is_edited ? 1 : 0;
        break;
    }
    return { image, name: filename(image), value };
  });
  keyed.sort((a, b) => {
    let comparison: number;
    if (isNameSort) comparison = nameCollator.compare(a.name, b.name);
    else if (key === 'date_taken') {
      const dateA = a.image.exif?.DateTimeOriginal || '';
      const dateB = b.image.exif?.DateTimeOriginal || '';
      comparison = dateA !== dateB ? (dateA < dateB ? -1 : 1) : a.image.modified - b.image.modified;
    } else comparison = a.value - b.value;
    // Existing secondary filename ordering is ascending in either direction.
    if (comparison === 0 && key !== 'name') return nameCollator.compare(a.name, b.name);
    return order === SortDirection.Ascending ? comparison : -comparison;
  });
  const files = keyed.map(({ image }) => image);
  orderings.set(images, { key, order, ratings, files });
  return files;
}

export function computeGroupedLibrary(libraryState: LibraryQueryState, settingsState: QuerySettings): GroupedLibrary {
  const {
    imageList, imageRatings, imageFlags = {}, filterCriteria, searchCriteria, sortCriteria, catalogMatches = null,
  } = libraryState;
  const { appSettings } = settingsState;

  const groupingMode: GroupingMode = appSettings?.grouping ?? 'off';
  const isGroupingActive = groupingMode !== 'off';

  const matchesFilter = (image: ImageFile): boolean => {
    if (filterCriteria.rating !== 0) {
      const rating = imageRatings[image.path] ?? image.rating;
      if (filterCriteria.rating === -1 && rating !== 0) return false;
      if (filterCriteria.rating === 5 && rating !== 5) return false;
      if (filterCriteria.rating > 0 && filterCriteria.rating < 5 && rating < filterCriteria.rating) return false;
    }

    if (filterCriteria.rawStatus && filterCriteria.rawStatus !== RawStatus.All) {
      if (filterCriteria.rawStatus === RawStatus.RawOnly && !image.is_raw) return false;
      if (filterCriteria.rawStatus === RawStatus.NonRawOnly && image.is_raw) return false;
    }

    if (filterCriteria.editedStatus && filterCriteria.editedStatus !== EditedStatus.All) {
      if (filterCriteria.editedStatus === EditedStatus.EditedOnly && !image.is_edited) return false;
      if (filterCriteria.editedStatus === EditedStatus.UneditedOnly && image.is_edited) return false;
    }

    if (filterCriteria.colors && filterCriteria.colors.length > 0) {
      const imageColor = (image.tags || []).find((tag: string) => tag.startsWith('color:'))?.substring(6);
      const hasMatchingColor = imageColor && filterCriteria.colors.includes(imageColor);
      const matchesNone = !imageColor && filterCriteria.colors.includes('none');

      if (!hasMatchingColor && !matchesNone) return false;
    }

    return true;
  };

  const { tags: searchTags, text: searchText, mode: searchMode } = searchCriteria;
  const lowerCaseSearchText = searchText.trim().toLowerCase();

  const parsedTags = searchTags.map((tag: string) => {
    const match = tag.match(ADVANCED_QUERY_REGEX);
    if (match) {
      const operator = match[2] || '=';
      return { type: 'query' as const, field: match[1].toLowerCase(), operator, value: match[3].toLowerCase(), raw: tag };
    }
    return { type: 'normal' as const, value: tag.toLowerCase(), raw: tag };
  });

  const evaluateQuery = (q: { field: string; operator: string; value: string }, image: ImageFile) => {
    const { field, operator, value } = q;

    if (['iso', 'aperture', 'f', 'shutter', 's', 'focal', 'mm', 'rating'].includes(field)) {
      let imgVal = 0;
      let qVal = parseFloat(value);

      if (field === 'iso')
        imgVal = parseInt(image.exif?.PhotographicSensitivity || image.exif?.ISOSpeedRatings || '0', 10) || 0;
      else if (field === 'aperture' || field === 'f') imgVal = parseAperture(image.exif?.FNumber);
      else if (field === 'focal' || field === 'mm') imgVal = parseFocalLength(image.exif?.FocalLength);
      else if (field === 'rating') imgVal = imageRatings[image.path] ?? image.rating;
      else if (field === 'shutter' || field === 's') {
        imgVal = parseShutter(image.exif?.ExposureTime);
        qVal = parseShutter(value);
      }

      switch (operator) {
        case '>':
          return imgVal > qVal;
        case '<':
          return imgVal < qVal;
        case '>=':
          return imgVal >= qVal;
        case '<=':
          return imgVal <= qVal;
        case '=':
        case ':':
          return imgVal === qVal;
        default:
          return false;
      }
    } else {
      let imgStr = '';
      if (field === 'camera' || field === 'make' || field === 'model') {
        imgStr = `${image.exif?.Make || ''} ${image.exif?.Model || ''}`.toLowerCase();
      } else if (field === 'lens') {
        imgStr = String(
          `${image.exif?.LensModel || ''} ${image.exif?.Lens || ''} ${image.exif?.LensMake || ''}`,
        ).toLowerCase();
      } else if (field === 'color') {
        imgStr = (image.tags || []).find((t: string) => t.startsWith('color:'))?.substring(6) || '';
      }

      return operator === '=' || operator === ':' ? imgStr.includes(value) : false;
    }
  };

  const isSearchActive = parsedTags.length > 0 || lowerCaseSearchText !== '' || catalogMatches !== null;

  const matchesSearch = (image: ImageFile): boolean => {
    if (!isSearchActive) return true;
    // In a library its catalog answered the search box and the metadata
    // lists (keywords, IPTC, camera, lens, … in one query); until its first
    // answer arrives, names and keywords are matched here.
    if (catalogMatches !== null) {
      if (!catalogMatches.has(photographPath(image.path))) return false;
      if (parsedTags.length === 0) return true;
    }

    const lowerCaseImageTags = (image.tags || []).map((t) => t.toLowerCase().replace('user:', ''));
    const lowerFilename = filename(image).toLowerCase();

    let tagsMatch = true;
    if (parsedTags.length > 0) {
      const evaluateTag = (parsedTag: (typeof parsedTags)[number]) => {
        if (parsedTag.type === 'normal') {
          return lowerCaseImageTags.some((imgTag) => imgTag.includes(parsedTag.value));
        }
        return evaluateQuery(parsedTag, image);
      };

      if (searchMode === 'OR') {
        tagsMatch = parsedTags.some(evaluateTag);
      } else {
        tagsMatch = parsedTags.every(evaluateTag);
      }
    }

    let textMatch = true;
    if (lowerCaseSearchText !== '' && catalogMatches === null) {
      textMatch =
        lowerFilename.includes(lowerCaseSearchText) || lowerCaseImageTags.some((t) => t.includes(lowerCaseSearchText));
    }

    return tagsMatch && textMatch;
  };

  let processedList = imageList;
  let badges: Map<GroupId, GroupBadgeInfo> | null = null;
  let searchMatchingGroupIds: Set<string> | null = null;

  if (isGroupingActive) {
    const groupEditedFiles = appSettings?.groupEditedFiles ?? true;
    const groupingResult = buildImageGroups(imageList, groupingMode, groupEditedFiles);
    processedList = groupingResult.displayList;
    badges = groupingResult.badges;

    if (isSearchActive) {
      searchMatchingGroupIds = new Set<string>();
      for (const image of imageList) {
        if (!image.group_id) continue;
        if (matchesSearch(image)) {
          searchMatchingGroupIds.add(image.group_id);
        }
      }
    }
  }

  const cached = cachedOrdering(imageList, imageRatings, sortCriteria);
  const ordered = cached ?? processedList;
  const groupPrimaries = cached && isGroupingActive ? new Set(processedList) : null;
  // Counts reflect the other active filters, so each number describes exactly
  // what choosing that flag will reveal. Filter the cached ordering in one pass.
  const flagCounts: FlagCounts = { all: 0, picked: 0, unflagged: 0, rejected: 0 };
  const flagFilter = filterCriteria.flag ?? 'all';
  const list: ImageFile[] = [];
  for (const image of ordered) {
    if (groupPrimaries && !groupPrimaries.has(image)) continue;
    if (!matchesFilter(image)) continue;
    const groupMatches = image.group_id && searchMatchingGroupIds?.has(image.group_id);
    if (isSearchActive && !groupMatches && !matchesSearch(image)) continue;
    const flag = flagName(imageFlag(imageFlags, image.path));
    flagCounts.all++;
    flagCounts[flag]++;
    if (flagFilter === 'all' || flagFilter === flag) list.push(image);
  }

  // A first query with restrictive filters only sorts its matches. Once the full
  // folder has been ordered, later filters consume that ordering directly.
  const displayList = cached
    ? list
    : orderedImages(list.length === imageList.length ? imageList : list, imageRatings, sortCriteria);
  return { displayList, badges, flagCounts };
}

export function computeSortedLibrary(libraryState: LibraryQueryState, settingsState: QuerySettings): ImageFile[] {
  return computeGroupedLibrary(libraryState, settingsState).displayList;
}

export function useSortedLibrary() {
  const imageList = useLibraryStore((state) => state.imageList);
  const imageRatings = useLibraryStore((state) => state.imageRatings);
  const imageFlags = useLibraryStore((state) => state.imageFlags);
  const filterCriteria = useLibraryStore((state) => state.filterCriteria);
  const searchCriteria = useLibraryStore((state) => state.searchCriteria);
  const sortCriteria = useLibraryStore((state) => state.sortCriteria);
  const catalogMatches = useLibraryStore((state) => state.catalogMatches);

  const appSettings = useSettingsStore((state) => state.appSettings);
  const grouping = appSettings?.grouping;
  const groupEditedFiles = appSettings?.groupEditedFiles;
  const needsExif = ['date_taken', 'iso', 'shutter_speed', 'aperture', 'focal_length'].includes(sortCriteria.key)
    || searchCriteria.tags.some((tag) => /^(iso|aperture|f|shutter|s|focal|mm|camera|make|model|lens)\s*(?::)?\s*(>=|<=|>|<|=)?\s*(.+)$/i.test(tag));
  useLibraryExif(needsExif);


  const result = useMemo(() => {
    const started = performanceDiagnosticsEnabled ? performance.now() : 0;
    const result = computeGroupedLibrary(
      { imageList, imageRatings, imageFlags, filterCriteria, searchCriteria, sortCriteria, catalogMatches },
      { appSettings: { grouping, groupEditedFiles } },
    );
    if (performanceDiagnosticsEnabled) {
      reportPerformance('library_query', {
        ms: performance.now() - started,
        assets: imageList.length,
        visible: result.displayList.length,
      });
    }
    return result;
  }, [
    imageList, sortCriteria, imageRatings, imageFlags, filterCriteria, searchCriteria, catalogMatches, grouping,
    groupEditedFiles,
  ]);

  return result;
}

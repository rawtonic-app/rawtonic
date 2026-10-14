import { create } from 'zustand';
import {
  FilterCriteria,
  ImageFile,
  RawStatus,
  SortCriteria,
  SortDirection,
  AlbumItem,
} from '../components/ui/AppProperties';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import type { FolderTree } from '../utils/folderTree';
import type { CatalogFoldersView } from '../utils/catalogFolders';

export interface NavHistoryItem {
  type: 'folder' | 'album';
  path: string;
  albumName?: string;
  images?: string[];
}

interface SearchCriteria {
  tags: string[];
  text: string;
  mode: 'AND' | 'OR';
}

interface LibraryState {
  // Paths & Trees
  rootPaths: string[];
  currentFolderPath: string | null;
  expandedFolders: Set<string>;
  folderTrees: FolderTree[];
  pinnedFolderTrees: FolderTree[];
  /** A catalog's folders by drive (null: not loaded, or no library). */
  catalogFolders: CatalogFoldersView | null;
  /** Disk trees of top-level catalog folders that are not library roots. */
  catalogTrees: FolderTree[];

  // Albums
  albumTree: AlbumItem[];
  activeAlbumId: string | null;
  expandedAlbumGroups: Set<string>;

  // Images & Selection
  imageList: Array<ImageFile>;
  imageRatings: Record<string, number>;
  imageFlags: Record<string, number>;
  sessionDirectory: string | null;
  multiSelectedPaths: Array<string>;
  selectionAnchorPath: string | null;
  libraryActivePath: string | null;
  libraryActiveAdjustments: Adjustments;

  // Sorting & Filtering
  sortCriteria: SortCriteria;
  filterCriteria: FilterCriteria;
  searchCriteria: SearchCriteria;

  // UI State specific to the Library View
  isTreeLoading: boolean;
  isViewLoading: boolean;
  libraryScrollTop: number;

  // Navigation History
  navHistory: NavHistoryItem[];
  navIndex: number;

  // Actions
  setLibrary: (updater: Partial<LibraryState> | ((state: LibraryState) => Partial<LibraryState>)) => void;
  clearSelection: () => void;
  setFilterCriteria: (criteria: Partial<FilterCriteria> | ((prev: FilterCriteria) => FilterCriteria)) => void;
  setSearchCriteria: (criteria: Partial<SearchCriteria> | ((prev: SearchCriteria) => SearchCriteria)) => void;
  setSortCriteria: (criteria: Partial<SortCriteria> | ((prev: SortCriteria) => SortCriteria)) => void;
  pushNavHistory: (item: NavHistoryItem) => void;
}

export const useLibraryStore = create<LibraryState>((set) => ({
  rootPaths: [],
  currentFolderPath: null,
  expandedFolders: new Set<string>(),
  folderTrees: [],
  pinnedFolderTrees: [],
  catalogFolders: null,
  catalogTrees: [],

  albumTree: [],
  activeAlbumId: null,
  expandedAlbumGroups: new Set<string>(),

  imageList: [],
  imageRatings: {},
  imageFlags: {},
  sessionDirectory: null,
  multiSelectedPaths: [],
  selectionAnchorPath: null,
  libraryActivePath: null,
  libraryActiveAdjustments: INITIAL_ADJUSTMENTS,

  sortCriteria: { key: 'name', order: SortDirection.Ascending },
  filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All },
  searchCriteria: { tags: [], text: '', mode: 'OR' },

  isTreeLoading: false,
  isViewLoading: false,
  libraryScrollTop: 0,

  navHistory: [],
  navIndex: -1,

  setLibrary: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  clearSelection: () => set({ multiSelectedPaths: [], libraryActivePath: null }),

  setFilterCriteria: (criteria) =>
    set((state) => ({
      filterCriteria:
        typeof criteria === 'function' ? criteria(state.filterCriteria) : { ...state.filterCriteria, ...criteria },
    })),

  setSearchCriteria: (criteria) =>
    set((state) => ({
      searchCriteria:
        typeof criteria === 'function' ? criteria(state.searchCriteria) : { ...state.searchCriteria, ...criteria },
    })),

  setSortCriteria: (criteria) =>
    set((state) => ({
      sortCriteria:
        typeof criteria === 'function' ? criteria(state.sortCriteria) : { ...state.sortCriteria, ...criteria },
    })),

  pushNavHistory: (item) =>
    set((state) => {
      const current = state.navHistory[state.navIndex];
      if (current && current.path === item.path && current.type === item.type) return state;

      const newHistory = state.navHistory.slice(0, state.navIndex + 1);
      newHistory.push(item);
      return { navHistory: newHistory, navIndex: newHistory.length - 1 };
    }),
}));

/** Whether the library lists the photo at `path` as a RAW (its recipe then
 * normalizes with the RAW defaults, as the backend reads it). */
export const isRawInLibrary = (path: string): boolean =>
  useLibraryStore.getState().imageList.find((image) => image.path === path)?.is_raw ?? false;

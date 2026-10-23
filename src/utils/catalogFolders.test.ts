import { expect, it } from 'vitest';
import {
  catalogTreePaths,
  formatSpace,
  graftMissing,
  isBelow,
  labelOnVolume,
  missingFolders,
  outermost,
  volumeSections,
  withMissingFolders,
  type CatalogFoldersView,
  type VolumeFolders,
} from './catalogFolders';
import type { FolderTree } from './folderTree';

const node = (path: string, children: FolderTree[] = [], extra: Partial<FolderTree> = {}): FolderTree => ({
  path,
  name: path.split('/').pop() ?? path,
  isDir: true,
  children,
  ...extra,
});

const volume = (extra: Partial<VolumeFolders>): VolumeFolders => ({
  key: 'id:X',
  id: 'X',
  name: 'Card',
  kind: 'local',
  mount_point: '/Volumes/Card',
  online: true,
  free_bytes: 50e9,
  total_bytes: 500e9,
  folders: [],
  roots: [],
  ...extra,
});

it('tells folders below others apart from look-alike names', () => {
  expect(isBelow('/a/b', '/a')).toBe(true);
  expect(isBelow('/a', '/a')).toBe(false);
  expect(isBelow('/ab', '/a')).toBe(false);
  expect(isBelow('C:\\Photos\\2026', 'C:\\Photos')).toBe(true);
  expect(isBelow('/Volumes/Card/x', '/')).toBe(true);
  expect(outermost(['/a/b', '/a', '/c', '/a/b/c', '/c'])).toEqual(['/a', '/c']);
  expect(labelOnVolume('/Volumes/Card/Shoots/2026', '/Volumes/Card')).toBe('Shoots/2026');
  expect(labelOnVolume('/Volumes/Card', '/Volumes/Card')).toBe('Card');
  expect(labelOnVolume('/Users/me/Pictures', '/')).toBe('Users/me/Pictures');
});

it('puts a missing folder at its place with any missing parents between', () => {
  const tree = node('/c', [node('/c/2026', [node('/c/2026/Day 1')])]);
  const grafted = graftMissing(tree, { path: '/c/2026/Trip/Day 3', count: 4 });
  const trip = grafted.children[0].children.find((child) => child.path === '/c/2026/Trip')!;
  expect(trip.missing).toBe(true);
  expect(trip.children[0]).toMatchObject({ path: '/c/2026/Trip/Day 3', missing: true, imageCount: 4 });
  // The original tree is not changed; folders on disk stay as they are.
  expect(tree.children[0].children).toHaveLength(1);
  expect(grafted.children[0].children[0]).toBe(tree.children[0].children[0]);
  // A folder on disk is never marked missing.
  expect(graftMissing(tree, { path: '/c/2026/Day 1', count: 1 }).children[0].children[0].missing).toBeUndefined();
  // Folders outside a tree are left for the Missing list.
  expect(withMissingFolders([tree], [{ path: '/elsewhere/x', count: 1 }])).toEqual([tree]);
});

it('lists catalog folders under their drive, connected or not', () => {
  const view: CatalogFoldersView = {
    catalog: true,
    volumes: [
      volume({
        key: 'id:B',
        name: 'Backup',
        mount_point: '/Volumes/Backup',
        online: false,
        free_bytes: null,
        total_bytes: null,
        folders: [
          { path: '/Volumes/Backup/2025/Day 1', photos: 3, offline: 3, exists: false },
          { path: '/Volumes/Backup/2025/Day 2', photos: 2, offline: 2, exists: false },
        ],
      }),
      volume({
        key: 'id:M',
        name: 'Macintosh HD',
        mount_point: '/',
        folders: [
          // Relinked outside the added folders: its own entry.
          { path: '/Users/me/Archive/Trip', photos: 1, offline: 0, exists: true },
          // Inside an added folder, gone from disk.
          { path: '/Users/me/Shoots/Old', photos: 2, offline: 2, exists: false },
          { path: '/Users/me/Shoots/Now', photos: 5, offline: 0, exists: true },
        ],
        roots: ['/Users/me/Shoots', '/Users/me/Lib.catalog'],
      }),
    ],
  };
  const rootPaths = ['/Users/me/Lib.catalog', '/Users/me/Shoots'];
  expect(catalogTreePaths(view, rootPaths)).toEqual(['/Users/me/Archive/Trip']);
  const disk = [
    node('/Users/me/Lib.catalog'),
    node('/Users/me/Shoots', [node('/Users/me/Shoots/Now')]),
    node('/Users/me/Archive/Trip'),
  ];
  const sections = volumeSections(view, disk, []);
  expect(sections.map((s) => s.volume.name)).toEqual(['Backup', 'Macintosh HD']);

  // A disconnected drive: its folders from the catalog, each listed.
  const backup = sections[0].trees;
  expect(backup.map((t) => [t.path, t.missing])).toEqual([
    ['/Volumes/Backup/2025/Day 1', true],
    ['/Volumes/Backup/2025/Day 2', true],
  ]);
  expect(backup[0].imageCount).toBe(3);

  const system = sections[1].trees;
  expect(system.map((t) => t.label)).toEqual(['Trip', 'Lib.catalog', 'Shoots']);
  const shoots = system.find((t) => t.path === '/Users/me/Shoots')!;
  expect(shoots.children.map((c) => [c.name, Boolean(c.missing)])).toEqual([
    ['Now', false],
    ['Old', true],
  ]);
  // Two top-level folders of the same name read as their place on the drive.
  const twins = volumeSections(
    {
      catalog: true,
      volumes: [
        volume({
          folders: [
            { path: '/Volumes/Card/A/2026', photos: 1, offline: 0, exists: true },
            { path: '/Volumes/Card/B/2026', photos: 1, offline: 0, exists: true },
          ],
        }),
      ],
    },
    [],
    [],
  );
  expect(twins[0].trees.map((t) => t.label)).toEqual(['A/2026', 'B/2026']);
  // A session has no drives.
  expect(volumeSections({ catalog: false, volumes: [] }, disk, [])).toEqual([]);
  expect(catalogTreePaths(null, rootPaths)).toEqual([]);
});

it('knows missing folders from offline photographs and from the catalog', () => {
  const view: CatalogFoldersView = {
    catalog: true,
    volumes: [volume({ folders: [{ path: '/Volumes/Card/X', photos: 2, offline: 2, exists: false }] })],
  };
  const found = missingFolders(view, [
    { path: '/s/Capture/Day 2', count: 1, missing: true, volume: null },
    { path: '/s/Capture', count: 1, missing: false, volume: null },
  ]);
  expect(found).toEqual([
    { path: '/s/Capture/Day 2', count: 1 },
    { path: '/Volumes/Card/X', count: 2 },
  ]);
});

it('shows free space in the larger unit', () => {
  expect(formatSpace(50e9, 500e9)).toBe('50 GB / 500 GB');
  expect(formatSpace(1.25e12, 4e12)).toBe('1.3 TB / 4 TB');
  expect(formatSpace(200e9, 1000e9)).toBe('200 GB / 1 TB');
  expect(formatSpace(null, 4e12)).toBeNull();
});

it('always shows one-root drive headings, startup first, then local drives and network shares by name', () => {
  const volumes = [
    volume({ key: 'share', name: 'A share', kind: 'network', roots: ['/mnt/share/Jobs'], online: false }),
    volume({ key: 'z', name: 'Zulu', kind: 'local', roots: ['/mnt/z/Jobs'] }),
    volume({ key: 'sys', name: 'Startup', kind: 'startup', roots: ['/home/me/Jobs'], mount_point: '/' }),
    volume({ key: 'a', name: 'Alpha', kind: 'removable', roots: ['/mnt/a/Jobs'] }),
  ];
  const sections = volumeSections({ catalog: true, volumes }, [], []);
  expect(sections.map(({ volume }) => volume.key)).toEqual(['sys', 'a', 'z', 'share']);
  expect(sections.every(({ trees }) => trees.length === 1)).toBe(true);
  expect(sections[3].volume.online).toBe(false);
  expect(sections[3].trees[0]).toMatchObject({ path: '/mnt/share/Jobs', missing: true });
  expect(volumes.map((volume) => volume.key)).toEqual(['share', 'z', 'sys', 'a']);
});

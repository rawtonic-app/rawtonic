import { expect, it } from 'vitest';
import { isInternalSessionFolder, withoutInternalSessionFolders } from './sessionFolders';

type Folder = { path: string; name: string; children: Folder[] };
const folder = (path: string, children: Folder[] = []): Folder => ({
  path,
  name: path.split('/').pop()!,
  children,
});

it('hides the open session’s masks folder in folder trees, only there', () => {
  const session = folder('/s/Job', [folder('/s/Job/Capture', [folder('/s/Job/Capture/masks')]), folder('/s/Job/masks')]);
  const other = folder('/s/Other', [folder('/s/Other/masks')]);
  const [shown, untouched] = withoutInternalSessionFolders([session, other], '/s/Job');
  expect(shown.children.map((child) => child.name)).toEqual(['Capture']);
  // A photograph folder called "masks" elsewhere stays visible.
  expect(shown.children[0].children.map((child) => child.name)).toEqual(['masks']);
  expect(untouched).toBe(other);
  expect(withoutInternalSessionFolders([session], null)[0].children).toHaveLength(2);
  expect(isInternalSessionFolder('/s/Job', 'masks', '/s/Job')).toBe(true);
  expect(isInternalSessionFolder('/s/Job/Capture', 'masks', '/s/Job')).toBe(false);
});

it('hides a catalog’s preview folders too', () => {
  expect(isInternalSessionFolder('/c/Main', 'previews', '/c/Main')).toBe(true);
  expect(isInternalSessionFolder('/c/Main', 'smart-previews', '/c/Main')).toBe(true);
  expect(isInternalSessionFolder('/c/Main', 'Capture', '/c/Main')).toBe(false);
});

import { expect, it } from 'vitest';
import { folderRefusalMessage, isInternalSessionFolder, parseFolderRefusal, withoutInternalSessionFolders } from './sessionFolders';

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

it('reads a folder refused for being (in) an Output folder, and says it in the user’s language', () => {
  // The backend's very payload (`a_refusal_travels_as_json_after_its_marker`
  // in crates/catalog/src/session_folders.rs), as the IPC error wraps it.
  const payload =
    'rawtonic-refusal:{"code":"inOutputFolder","folder":"/Volumes/Job: 1/Deliveries \\"A\\"","output":"/Volumes/Job: 1/Deliveries","role":"capture"}';
  const expected = {
    code: 'inOutputFolder',
    role: 'capture',
    folder: '/Volumes/Job: 1/Deliveries "A"',
    output: '/Volumes/Job: 1/Deliveries',
  };
  expect(parseFolderRefusal(payload)).toEqual(expected);
  expect(parseFolderRefusal(`Could not set the folder (${payload})`)).toEqual(expected);
  // Every other error, and none, is left to the caller as it is.
  expect(parseFolderRefusal('/job/x is not a folder')).toBeNull();
  expect(parseFolderRefusal('rawtonic-refusal:{"code":"inOutputFolder","role":"capture"}')).toBeNull();
  expect(parseFolderRefusal('rawtonic-refusal:not json')).toBeNull();

  const t = (key: string, options?: Record<string, unknown>) => {
    const role = /^session\.folders\.role\.(\w+)$/.exec(key);
    if (role) return role[1][0].toUpperCase() + role[1].slice(1);
    return `${key}:${JSON.stringify(options)}`;
  };
  const refusal = (role: string, folder: string, output: string) =>
    `rawtonic-refusal:${JSON.stringify({ code: 'inOutputFolder', folder, output, role })}`;
  const message = folderRefusalMessage(t, refusal('selects', '/job/Deliveries', '/job/Output'), true);
  expect(message).toBe(
    'session.folders.inOutputFolder:{"folder":"/job/Deliveries","output":"/job/Output","role":"Selects"}',
  );
  // A former Output folder that is gone held the exports.
  const gone = folderRefusalMessage(t, refusal('capture', '/job/Deliveries', '/job/Deliveries'), false);
  expect(gone).toContain('session.folders.inOutputFolderNotThere:');
  expect(folderRefusalMessage(t, 'something else', true)).toBeNull();
});

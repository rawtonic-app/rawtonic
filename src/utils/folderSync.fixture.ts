import type { SyncMove, SyncPlan } from './folderSync';

/** A scan result with one row in every group (tests). */
export const identity = (n: number) => ({ size: n, mtime: n, fingerprint: `fp${n}` });
const move = (asset: string, from: string, to: string, verified: boolean): SyncMove => ({
  assetId: asset,
  from,
  to,
  identity: identity(1),
  verified,
});

export function samplePlan(): SyncPlan {
  return {
    folders: ['/lib/Capture'],
    files: 9,
    verifiedMoves: [move('a', '/lib/Capture/A.ARW', '/lib/Capture/Day 2/A.ARW', true)],
    probableMoves: [move('b', '/lib/Capture/B.ARW', '/lib/Capture/B2.ARW', false)],
    newFiles: [{ path: '/lib/Capture/N.ARW', identity: identity(2), copyOf: [] }],
    copies: [{ path: '/lib/Capture/C copy.ARW', identity: identity(3), copyOf: ['/lib/Capture/C.ARW'] }],
    contested: [
      {
        path: '/lib/Capture/X.ARW',
        identity: identity(4),
        moves: [move('x', '/lib/Capture/old/X.ARW', '/lib/Capture/X.ARW', true)],
        copyOf: ['/lib/Capture/Y.ARW'],
      },
    ],
    removed: [{ assetId: 'r', path: '/lib/Capture/R.ARW', volume: null }],
    unavailable: [{ assetId: 'u', path: '/Volumes/Photos/U.ARW', volume: 'Photos' }],
    errors: [],
  };
}


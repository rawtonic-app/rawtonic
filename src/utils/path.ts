// SPDX-License-Identifier: AGPL-3.0-or-later

export function pathSeparator(base: string): '/' | '\\' {
  return base.startsWith('\\\\?\\') || (base.includes('\\') && !base.includes('/')) ? '\\' : '/';
}

/** Append relative components using the base's spelling. Preserve its root
 * and prefix, and leave dots and other filename characters alone. */
export function joinPath(base: string, ...parts: string[]): string {
  const separator = pathSeparator(base);
  const windowsDrive = /^[A-Za-z]:[\\/]/.test(base);
  const native = (value: string) =>
    separator === '\\' ? value.replaceAll('/', '\\') : windowsDrive ? value.replaceAll('\\', '/') : value;
  const leading = separator === '\\' ? /^\\+/ : /^\/+/;
  const trailing = separator === '\\' ? /\\+$/ : /\/+$/;
  let path = native(base);
  for (const part of parts) {
    const relative = native(part).replace(leading, '').replace(trailing, '');
    if (relative) path = path ? `${path.replace(trailing, '')}${separator}${relative}` : relative;
  }
  return path;
}

/** Parent of a file or folder, with its drive or POSIX root intact. */
export function dirname(path: string): string {
  const slash = path.lastIndexOf('/');
  const cut = path.startsWith('/') ? slash : Math.max(slash, path.lastIndexOf('\\'));
  if (cut < 0) return '';
  const parent = path.slice(0, cut);
  return cut === 0 || /^(?:\\\\\?\\)?[A-Za-z]:$/.test(parent) ? path.slice(0, cut + 1) : parent;
}

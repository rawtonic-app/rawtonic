/** Interface chrome shades. Legacy `theme` settings are still ignored. */
export type InterfaceShade = 'darkest' | 'dark-grey' | 'medium-grey';

export function interfaceShade(value: unknown): InterfaceShade {
  return value === 'dark-grey' || value === 'medium-grey' ? value : 'darkest';
}

/** Set before publishing settings so the native viewer reads the new chrome
 * colour in the same React commit. No photo or renderer data changes here. */
export function applyInterfaceShade(value: unknown): void {
  document.documentElement.dataset.interfaceShade = interfaceShade(value);
}

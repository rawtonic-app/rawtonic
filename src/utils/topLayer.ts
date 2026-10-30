/** Where an overlay opened now must render: a modal <dialog> sits in the
 * top layer and makes the rest of the document inert, so an overlay opened
 * while one is open (a menu, a confirmation) renders inside the last one. */
export function overlayHost(): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>('dialog[open]')).at(-1) ?? document.body;
}

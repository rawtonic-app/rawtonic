/** Swallows the mouse down/up/click that follow a pointer down which only
 * closed a menu or panel (the browser sends them even when the pointer down
 * was stopped), so the press does not also zoom or act on the photograph.
 * Ends at the click or after half a second. */
export function swallowNextClick(
  events: readonly string[] = ['mousedown', 'mouseup', 'click', 'dblclick'],
) {
  const stop = (event: Event) => {
    event.preventDefault();
    event.stopPropagation();
    if (event.type === 'click') done();
  };
  const done = () => {
    for (const type of events) window.removeEventListener(type, stop, true);
  };
  for (const type of events) window.addEventListener(type, stop, true);
  window.setTimeout(done, 500);
}

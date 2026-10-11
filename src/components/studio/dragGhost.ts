/** What follows the pointer while something is dragged: a label, optionally
 * with a thumbnail and a count badge (Lightroom's dragged photo stack). */
export function createDragGhost({
  label,
  image,
  count = 1,
  kind,
}: {
  label: string;
  image?: string;
  count?: number;
  kind?: string;
}) {
  const ghost = document.createElement("div");
  ghost.className = `studio-drag-ghost ${kind ? `is-${kind}` : ""} ${count > 1 ? "is-stack" : ""}`;
  if (image) {
    const thumb = document.createElement("img");
    thumb.src = image;
    thumb.alt = "";
    ghost.append(thumb);
  }
  const text = document.createElement("span");
  text.textContent = label;
  ghost.append(text);
  if (count > 1 && image) {
    const badge = document.createElement("b");
    badge.textContent = String(count);
    ghost.append(badge);
  }
  document.body.append(ghost);
  document.body.classList.add("is-dragging");
  return {
    move(x: number, y: number) {
      ghost.style.transform = `translate(${x + 12}px, ${y + 8}px)`;
    },
    remove() {
      ghost.remove();
      document.body.classList.remove("is-dragging");
    },
  };
}

import type { KeyboardEvent, ReactNode } from "react";
import { useDraggable, useDroppable } from "@dnd-kit/core";

/** A chip of the format field (token `index`) or a token from the lists. */
export type NamingDragData =
  | { kind: "chip"; index: number; text: string }
  | { kind: "token"; text: string };

export function NamingDragButton({
  id,
  data,
  children,
  onClick,
  onDoubleClick,
  onKeyDown,
  className,
  title,
  label,
  buttonRef,
}: {
  id: string;
  data: NamingDragData;
  children: ReactNode;
  onClick?: () => void;
  onDoubleClick?: () => void;
  onKeyDown?: (event: KeyboardEvent<HTMLButtonElement>) => void;
  className?: string;
  title?: string;
  label?: string;
  buttonRef?: (node: HTMLButtonElement | null) => void;
}) {
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id,
    data,
  });
  return (
    <button
      type="button"
      ref={(node) => {
        setNodeRef(node);
        buttonRef?.(node);
      }}
      {...attributes}
      {...listeners}
      aria-label={label}
      title={title}
      className={`${className ?? ""} naming-draggable${isDragging ? " is-dragging" : ""}`}
      onClick={onClick}
      onDoubleClick={onDoubleClick}
      onKeyDown={onKeyDown}
    >
      {children}
    </button>
  );
}

/** Free text `index` of the format field: a dropped token lands at its end
 * (before token `index`). */
export function NamingDropSlot({
  index,
  children,
}: {
  index: number;
  children?: ReactNode;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: `slot-${index}`,
    data: { index },
  });
  return (
    <span
      ref={setNodeRef}
      className={`naming-drop-slot${isOver ? " is-over" : ""}`}
      data-drop-index={index}
    >
      {children}
    </span>
  );
}

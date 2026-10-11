import type { ReactNode } from "react";
import { useDraggable, useDroppable } from "@dnd-kit/core";

export type NamingDragData =
  | { kind: "part"; index: number; text: string }
  | { kind: "token"; text: string };

export function NamingDragButton({
  id,
  data,
  children,
  onClick,
  pressed,
  className,
  title,
}: {
  id: string;
  data: NamingDragData;
  children: ReactNode;
  onClick: () => void;
  pressed?: boolean;
  className?: string;
  title?: string;
}) {
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id,
    data,
  });
  return (
    <button
      type="button"
      ref={setNodeRef}
      {...attributes}
      {...listeners}
      aria-pressed={pressed}
      title={title}
      className={`${className ?? ""} naming-draggable${isDragging ? " is-dragging" : ""}`}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/** Dropping on a block inserts before it; the final target appends. */
export function NamingDropSlot({
  index,
  children,
  label,
  dragging,
}: {
  index: number;
  children?: ReactNode;
  label: string;
  dragging: boolean;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: `slot-${index}`,
    data: { index },
  });
  return (
    <div
      ref={setNodeRef}
      className={`naming-drop-slot${isOver ? " is-over" : ""}${!children ? " is-end" : ""}`}
      data-drop-index={index}
    >
      {children ?? <span>{dragging ? label : "+"}</span>}
    </div>
  );
}

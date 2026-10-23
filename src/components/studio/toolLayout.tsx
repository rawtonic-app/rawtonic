import { swallowDragClick } from '../../utils/swallowClick';
import {
  Children,
  createContext,
  isValidElement,
  memo,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactElement,
  type ReactNode,
} from "react";
import { ArrowDown, ArrowUp, ChevronsDownUp, ChevronsUpDown, RotateCcw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorStore } from "../../store/useEditorStore";
import { useContextMenu } from "../../context/ContextMenuContext";
import { OPTION_SEPARATOR, type ToolLayout } from "../ui/AppProperties";
import { createDragGhost } from "./dragGhost";

/** The tool column a tool group belongs to ("library", "develop:color", …);
 * open state and order are remembered per column. */
const ToolColumnContext = createContext<{
  scope: string;
  menu?(e: React.MouseEvent): void;
}>({ scope: "tools" });

/** The tool slot (`scope/id`) a press went down in, until it is released. */
let pressedSlot: string | null = null;
const releasePress = () => {
  pressedSlot = null;
};

/** A tool's content. While a slider drag that started in another tool is in
 * progress it keeps its last render: the drag's commits (every 250 ms,
 * utils/sliderDraft.ts) then reach only the dragged tool, which holds the
 * displays linked to the dragged value. It renders again when the drag ends.
 * Components that subscribe to the editor store themselves still render on
 * their own; those in Develop's tools select only the values they show
 * (lens correction, face retouch and the people mask still take the whole
 * recipe). */
const ToolSlotContent = memo(
  function ToolSlotContent({ children }: { children: ReactNode; frozen: boolean }) {
    return <>{children}</>;
  },
  (_, next) => next.frozen,
);

/** Pointer travel before a press on a tool header becomes a drag. */
const DRAG_THRESHOLD = 5;

function saveLayout(update: (layout: ToolLayout) => ToolLayout) {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings) return;
  void handleSettingsChange({ ...appSettings, toolLayout: update(appSettings.toolLayout ?? {}) });
}

/** Whether a tool is open: the remembered state of this column, else
 * `defaultOpen`. Remembered across Library/Develop switches and restarts. */
export function useToolOpen(title: string, defaultOpen: boolean) {
  const { scope } = useContext(ToolColumnContext);
  const key = `${scope}/${title}`;
  const stored = useSettingsStore((s) => s.appSettings?.toolLayout?.open?.[key]);
  // Without settings (not loaded yet) the choice still holds for this view.
  const [local, setLocal] = useState<boolean | null>(null);
  const open = stored ?? local ?? defaultOpen;
  const setOpen = (next: boolean) => {
    setLocal(next);
    saveLayout((layout) => ({ ...layout, open: { ...layout.open, [key]: next } }));
  };
  return [open, setOpen] as const;
}

/** Context menu for a tool header in a column (move, open/close all). */
export function useToolMenu() {
  return useContext(ToolColumnContext).menu;
}

/** `defaults` in the user's `stored` order; tools the stored order does not
 * know keep their default place after their default predecessor. */
export function mergeToolOrder(defaults: string[], stored?: string[]) {
  if (!stored?.length) return defaults;
  const present = new Set(defaults);
  const result = [...new Set(stored)].filter((id) => present.has(id));
  defaults.forEach((id, i) => {
    if (result.includes(id)) return;
    let at = 0;
    for (let j = i - 1; j >= 0; j--) {
      const k = result.indexOf(defaults[j]);
      if (k >= 0) {
        at = k + 1;
        break;
      }
    }
    result.splice(at, 0, id);
  });
  return result;
}

/** `order` with the stored tools that are not shown right now kept after
 * their stored predecessor, so hiding a tool does not forget its place. */
export function keepAbsentTools(order: string[], stored?: string[]) {
  if (!stored?.length) return order;
  const result = [...order];
  stored.forEach((id, i) => {
    if (result.includes(id)) return;
    let at = 0;
    for (let j = i - 1; j >= 0; j--) {
      const k = result.indexOf(stored[j]);
      if (k >= 0) {
        at = k + 1;
        break;
      }
    }
    result.splice(at, 0, id);
  });
  return result;
}

const toolId = (child: ReactElement) => String(child.key).replace(/^\.\$/, "");

/** A column of tool groups (keyed children) the user can reorder by dragging
 * a tool header or from its context menu, as in Capture One's tool tabs.
 * Open state and order are saved in the app settings per `scope`. */
export function ToolColumn({
  scope,
  className,
  children,
}: {
  scope: string;
  className?: string;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const items = Children.toArray(children).filter(isValidElement) as ReactElement[];
  const byId = new Map(items.map((child) => [toolId(child), child]));
  const stored = useSettingsStore((s) => s.appSettings?.toolLayout?.order?.[scope]);
  const order = mergeToolOrder([...byId.keys()], stored);
  const ref = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ id: string; before: string | null } | null>(null);
  const { showContextMenu } = useContextMenu();
  const sliderDragging = useEditorStore((s) => s.isSliderDragging);
  // Releases end the press wherever they happen; a press outside any tool
  // (the photo, a shortcut) freezes nothing.
  useEffect(() => {
    window.addEventListener("pointerup", releasePress, true);
    window.addEventListener("pointercancel", releasePress, true);
    window.addEventListener("pointerdown", releasePress, true);
    return () => {
      window.removeEventListener("pointerup", releasePress, true);
      window.removeEventListener("pointercancel", releasePress, true);
      window.removeEventListener("pointerdown", releasePress, true);
    };
  }, []);
  const frozen = (id: string) =>
    sliderDragging && pressedSlot !== null && pressedSlot !== `${scope}/${id}`;

  const saveOrder = (next: string[] | null) =>
    saveLayout((layout) => {
      const all = { ...layout.order };
      if (next) all[scope] = keepAbsentTools(next, layout.order?.[scope]);
      else delete all[scope];
      return { ...layout, order: all };
    });
  const move = (id: string, to: number) => {
    const next = order.filter((other) => other !== id);
    next.splice(Math.max(0, Math.min(to, next.length)), 0, id);
    saveOrder(next);
  };
  const setAllOpen = (open: boolean) =>
    saveLayout((layout) => {
      const titles = [...(ref.current?.querySelectorAll<HTMLElement>("[data-tool-title]") ?? [])]
        .filter((el) => el.closest("[data-tool-column]") === ref.current)
        .map((el) => el.dataset.toolTitle!);
      const next = { ...layout.open };
      for (const title of titles) next[`${scope}/${title}`] = open;
      return { ...layout, open: next };
    });

  const slotOf = (el: Element | null) => {
    const slot = el?.closest<HTMLElement>("[data-tool-slot]");
    return slot && slot.parentElement === ref.current ? slot : null;
  };
  const menu = (e: React.MouseEvent) => {
    const id = slotOf(e.currentTarget as Element)?.dataset.toolSlot;
    if (!id) return;
    e.preventDefault();
    const at = order.indexOf(id);
    showContextMenu(e.clientX, e.clientY, [
      {
        label: t("studio.develop.layout.moveUp"),
        icon: ArrowUp,
        disabled: at <= 0,
        onClick: () => move(id, at - 1),
      },
      {
        label: t("studio.develop.layout.moveDown"),
        icon: ArrowDown,
        disabled: at >= order.length - 1,
        onClick: () => move(id, at + 1),
      },
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.develop.layout.expandAll"),
        icon: ChevronsUpDown,
        onClick: () => setAllOpen(true),
      },
      {
        label: t("studio.develop.layout.collapseAll"),
        icon: ChevronsDownUp,
        onClick: () => setAllOpen(false),
      },
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.develop.layout.restoreOrderMenu"),
        icon: RotateCcw,
        disabled: !stored,
        onClick: () => saveOrder(null),
      },
    ]);
  };

  // Drop position: before the first slot whose middle is below the pointer.
  const beforeAt = (y: number, dragged: string) => {
    const slots = [...(ref.current?.children ?? [])] as HTMLElement[];
    for (const slot of slots) {
      const id = slot.dataset.toolSlot;
      if (!id || id === dragged) continue;
      const rect = slot.getBoundingClientRect();
      if (y < rect.top + rect.height / 2) return id;
    }
    return null;
  };
  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const target = e.target as Element;
    const summary = target.closest("summary");
    if (!summary || target.closest("button, input, select, textarea")) return;
    const slot = slotOf(summary);
    const id = slot?.dataset.toolSlot;
    if (!slot || !id || slot.querySelector("summary") !== summary) return;
    const pointer = e.pointerId;
    const start = { x: e.clientX, y: e.clientY };
    let ghost: ReturnType<typeof createDragGhost> | null = null;
    let before: string | null = null;
    const moveTo = (ev: PointerEvent) => {
      if (ev.pointerId !== pointer) return;
      if (!ghost) {
        if (Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < DRAG_THRESHOLD) return;
        ghost = createDragGhost({ label: summary.querySelector("span")?.textContent ?? id, kind: "tool" });
      }
      ghost.move(ev.clientX, ev.clientY);
      before = beforeAt(ev.clientY, id);
      setDrag({ id, before });
    };
    const end = (ev: PointerEvent) => {
      if (ev.pointerId !== pointer) return;
      document.removeEventListener("pointermove", moveTo, true);
      document.removeEventListener("pointerup", end, true);
      document.removeEventListener("pointercancel", end, true);
      if (!ghost) return;
      ghost.remove();
      setDrag(null);
      // The press ended a drag, not a click that opens or closes the tool.
      swallowDragClick();
      if (ev.type !== "pointerup") return;
      const rest = order.filter((other) => other !== id);
      const at = before === null ? rest.length : rest.indexOf(before);
      rest.splice(at, 0, id);
      if (rest.some((other, i) => other !== order[i])) saveOrder(rest);
    };
    document.addEventListener("pointermove", moveTo, true);
    document.addEventListener("pointerup", end, true);
    document.addEventListener("pointercancel", end, true);
  };

  // While dragging, the tools already take their new places, so the column
  // shows the result before the drop (the dragged tool is outlined).
  const shown = drag
    ? (() => {
        const rest = order.filter((id) => id !== drag.id);
        rest.splice(drag.before === null ? rest.length : rest.indexOf(drag.before), 0, drag.id);
        return rest;
      })()
    : order;
  // One context value per column: a new one would re-render every tool,
  // frozen ones included.
  const menuRef = useRef(menu);
  menuRef.current = menu;
  const context = useMemo(
    () => ({ scope, menu: (e: React.MouseEvent) => menuRef.current(e) }),
    [scope],
  );
  return (
    <ToolColumnContext.Provider value={context}>
      <div
        ref={ref}
        className={`studio-tool-column ${className ?? ""} ${drag ? "is-sorting" : ""}`}
        data-tool-column={scope}
        onPointerDown={onPointerDown}
        onPointerDownCapture={(e) => {
          const id = slotOf(e.target as Element)?.dataset.toolSlot;
          if (id) pressedSlot = `${scope}/${id}`;
        }}
      >
        {shown.map((id) => (
          <div
            key={id}
            data-tool-slot={id}
            className={`studio-tool-slot ${drag?.id === id ? "is-dragged" : ""}`}
          >
            <ToolSlotContent frozen={frozen(id)}>{byId.get(id)}</ToolSlotContent>
          </div>
        ))}
        {stored && (
          <button
            type="button"
            className="studio-text-button studio-tool-order-reset"
            title={t("studio.develop.layout.restoreOrderTitle")}
            onClick={() => saveOrder(null)}
          >
            <RotateCcw size={11} />
            {t("studio.develop.layout.restoreOrder")}
          </button>
        )}
      </div>
    </ToolColumnContext.Provider>
  );
}

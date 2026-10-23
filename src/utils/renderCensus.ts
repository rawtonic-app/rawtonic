/**
 * Profiling builds: counts which components rendered in each React commit,
 * through the hook React DevTools uses (installed before react-dom loads, so
 * main.tsx imports this module first). usePerformanceDiagnostics reports the
 * counts of a pointer session, to find what re-renders during a slider drag.
 */

interface Fiber {
  type: unknown;
  elementType: unknown;
  tag: number;
  flags: number;
  child: Fiber | null;
  sibling: Fiber | null;
  alternate: Fiber | null;
  return: Fiber | null;
}

// react-reconciler's PerformedWork flag and the fiber tags of user components
// (function, class, forwardRef, memo, simple memo).
const PERFORMED_WORK = 1;
const COMPONENT_TAGS = new Set([0, 1, 11, 14, 15]);

let counting: Record<string, number> | null = null;

/** Starts counting renders per component (replacing an earlier count). */
export function startRenderCensus() {
  counting = {};
}

/** Stops counting and returns the renders per component name. */
export function takeRenderCensus(): Record<string, number> {
  const counts = counting ?? {};
  counting = null;
  return counts;
}

function ownName(fiber: Fiber): string | undefined {
  const type = (fiber.elementType ?? fiber.type) as
    | { displayName?: string; name?: string; type?: { displayName?: string; name?: string }; render?: { name?: string } }
    | undefined;
  return (
    type?.displayName ||
    type?.name ||
    type?.type?.displayName ||
    type?.type?.name ||
    type?.render?.name ||
    undefined
  );
}

/** The component's name; an anonymous one is named after its nearest named
 * parent component. */
function nameOf(fiber: Fiber): string {
  const own = ownName(fiber);
  if (own) return own;
  for (let parent = fiber.return; parent; parent = parent.return) {
    const name = COMPONENT_TAGS.has(parent.tag) ? ownName(parent) : undefined;
    if (name) return `(anonymous in ${name})`;
  }
  return "(anonymous)";
}

function visit(fiber: Fiber | null, counts: Record<string, number>) {
  for (let node = fiber; node; node = node.sibling) {
    const previous = node.alternate;
    const rendered = !previous || (node.flags & PERFORMED_WORK) !== 0;
    if (rendered && COMPONENT_TAGS.has(node.tag)) {
      const name = nameOf(node);
      counts[name] = (counts[name] ?? 0) + 1;
    }
    // A subtree React skipped keeps the previous fibers.
    if (!previous || node.child !== previous.child) visit(node.child, counts);
  }
}

if (import.meta.env.VITE_RAWTONIC_PERF === "1" && typeof window !== "undefined") {
  const target = window as unknown as { __REACT_DEVTOOLS_GLOBAL_HOOK__?: object };
  target.__REACT_DEVTOOLS_GLOBAL_HOOK__ ??= {
    supportsFiber: true,
    renderers: new Map(),
    inject: () => 1,
    checkDCE: () => {},
    onScheduleFiberRoot: () => {},
    onCommitFiberUnmount: () => {},
    onPostCommitFiberRoot: () => {},
    onCommitFiberRoot: (_id: number, root: { current: Fiber }) => {
      if (!counting) return;
      counting["(commits)"] = (counting["(commits)"] ?? 0) + 1;
      visit(root.current.child, counting);
    },
  };
}

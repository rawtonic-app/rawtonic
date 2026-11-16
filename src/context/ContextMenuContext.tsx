import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  CSSProperties,
  ReactNode,
  RefObject
} from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { Check, ChevronRight } from 'lucide-react';
import { Option, OPTION_HEADER, OPTION_SEPARATOR } from '../components/ui/AppProperties';
import clsx from 'clsx';
import { overlayHost } from '../utils/topLayer';

/**
 * Keys while a menu is open (Windows/KDE menus, UX4-06, UXR4L-04): ↑/↓
 * Home/End move between rows, → or Enter opens a submenu at its first row,
 * ← and Escape close one level, Enter/Space choose, a letter goes to the
 * next row starting with it. The menu keeps every key from the app's
 * shortcuts; ⌘/Ctrl combos close it and go on. One window listener in the
 * capture phase, added when this module loads: before any of the app's.
 */
let menuKeys: ((event: KeyboardEvent) => void) | null = null;
/** The menu (path "" = the top one, "2" = the third row's submenu) whose
 * first row takes the focus once it shows. */
let focusFirstIn: string | null = null;
if (typeof window !== 'undefined') {
  window.addEventListener('keydown', (event) => menuKeys?.(event), true);
}

const menuRows = (panel: Element): HTMLButtonElement[] =>
  Array.from(panel.querySelectorAll<HTMLButtonElement>(':scope > div > button.rawtonic-menu-item:not([disabled])'));

const panelOf = (path: string): HTMLElement | null =>
  document.querySelector<HTMLElement>(`[role="menu"][data-menu-path="${path}"]`);

/** Focuses the first row of the menu at `path` if one was asked for. */
function focusFirstRow(path: string): void {
  if (focusFirstIn !== path) return;
  const panel = panelOf(path);
  const first = panel && menuRows(panel)[0];
  if (!first) return;
  focusFirstIn = null;
  first.focus({ preventScroll: true });
}

interface ContextMenuProviderProps {
  children: ReactNode;
}

interface MenuItemProps {
  hideContextMenu(): void;
  path: number[];
  option: Option;
}

interface SubMenuProps {
  cancelCloseSubmenu(): void;
  closeSubmenu(path: number[]): void;
  hideContextMenu(): void;
  options: Array<Option>;
  parentRef: RefObject<HTMLButtonElement | null>;
  parentPath: number[];
}

interface MenuState {
  isVisible: boolean;
  x: number;
  y: number;
  /** Where the menu was asked for, before keeping it inside the window. */
  requested?: { x: number; y: number };
  options: Array<Option>;
}

export interface ContextMenuContextValue {
  activeSubmenu: number[] | null;
  cancelCloseSubmenu(): void;
  closeSubmenu(path: number[]): void;
  hideContextMenu(): void;
  menuId: number;
  menuRef: RefObject<HTMLDivElement | null>;
  menuState: MenuState;
  openSubmenu(path: number[] | null): void;
  /** `focusFirst`: opened from the keyboard (F10, Alt), its first row takes the focus. */
  showContextMenu(x: number, y: number, options: Array<Option>, open?: { focusFirst?: boolean }): void;
}

const ContextMenuContext = createContext<ContextMenuContextValue | 'dark'>('dark');

export const useContextMenu = (): ContextMenuContextValue => {
  // 'dark' is only the inherited placeholder default; menus are used inside ContextMenuProvider.
  return useContext(ContextMenuContext) as ContextMenuContextValue;
};

function SubMenu({ cancelCloseSubmenu, closeSubmenu, hideContextMenu, options, parentRef, parentPath }: SubMenuProps) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [style, setStyle] = useState<CSSProperties>({ opacity: 0 });
  const [safeAreaPath, setSafeAreaPath] = useState<string | null>(null);
  const [menuHeight, setMenuHeight] = useState(0);

  const customOption = options.length === 1 && options[0].customComponent ? options[0] : null;
  const CustomComponent = customOption?.customComponent;
  const isInteractiveSubmenu = Boolean(customOption);

  useEffect(() => {
    if (!menuRef.current) {
      return;
    }
    const resizeObserver = new ResizeObserver(([entry]) => setMenuHeight(entry.contentRect.height));
    resizeObserver.observe(menuRef.current);
    return () => resizeObserver.disconnect();
  }, []);

  useLayoutEffect(() => {
    if (parentRef?.current && menuRef?.current) {
      const parentRect = parentRef.current.getBoundingClientRect();
      const menuEl = menuRef.current;

      const subMenuWidth = menuEl?.offsetWidth || 256;
      const subMenuHeight = menuEl?.offsetHeight || 100;

      if (subMenuWidth === 0 || subMenuHeight === 0) {
        return;
      }

      let top = parentRect.top;
      let left = parentRect.right - 4;

      if (left + subMenuWidth > window.innerWidth) {
        left = parentRect.left - subMenuWidth + 4;
      }
      if (left < 0) {
        left = 5;
      }

      if (top + subMenuHeight > window.innerHeight) {
        top = window.innerHeight - subMenuHeight - 5;
      }
      if (top < 0) {
        top = 5;
      }

      setStyle({ top: `${top}px`, left: `${left}px`, opacity: 1 });
      focusFirstRow(parentPath.join('.'));

      const isRightSide = left >= parentRect.right - 10;
      let path: string;

      if (isRightSide) {
        path = `
          M ${parentRect.right} ${parentRect.top}
          L ${left} ${top}
          L ${left} ${top + subMenuHeight}
          L ${parentRect.right} ${parentRect.bottom}
          Z
        `;
      } else {
        path = `
          M ${parentRect.left} ${parentRect.top}
          L ${left + subMenuWidth} ${top}
          L ${left + subMenuWidth} ${top + subMenuHeight}
          L ${parentRect.left} ${parentRect.bottom}
          Z
        `;
      }
      setSafeAreaPath(path);
    }
  }, [parentRef, options, menuHeight, parentPath]);

  const menuMarkup = (
    <>
      {safeAreaPath && (
        <svg
          className="fixed top-0 left-0 w-full h-full pointer-events-none z-100"
          style={{ width: '100vw', height: '100vh' }}
        >
          <path
            d={safeAreaPath}
            fill="transparent"
            className="pointer-events-auto cursor-default"
            onMouseEnter={cancelCloseSubmenu}
          />
        </svg>
      )}

      <motion.div
        animate={{ opacity: 1, scale: 1 }}
        className="fixed z-101"
        exit={{ opacity: 0, scale: 0.95 }}
        initial={{ opacity: 0, scale: 0.95 }}
        onContextMenu={(e: React.MouseEvent) => e.preventDefault()}
        onMouseEnter={cancelCloseSubmenu}
        onMouseLeave={() => {
          if (!isInteractiveSubmenu) {
            closeSubmenu(parentPath);
          }
        }}
        ref={menuRef}
        style={style}
        transition={{ duration: 0.1, ease: 'easeOut' }}
      >
        <div
          className={clsx('rawtonic-menu', !CustomComponent && 'rawtonic-menu-list min-w-56')}
          data-menu-path={parentPath.join('.')}
          role="menu"
        >
          {CustomComponent && customOption ? (
            <CustomComponent {...customOption.customProps} hideContextMenu={hideContextMenu} />
          ) : (
            options.map((option: Option, index: number) => (
              <MenuItem hideContextMenu={hideContextMenu} key={index} option={option} path={[...parentPath, index]} />
            ))
          )}
        </div>
      </motion.div>
    </>
  );

  return createPortal(menuMarkup, overlayHost());
}

function MenuItem({ option, path, hideContextMenu }: MenuItemProps) {
  const { activeSubmenu, openSubmenu, closeSubmenu, cancelCloseSubmenu } = useContextMenu();
  const itemRef = useRef<HTMLButtonElement>(null);
  const hoverTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hasInteractiveSubmenu = Boolean(option.submenu?.length === 1 && option.submenu[0].customComponent);
  // A hover's pending submenu change dies with the item (a menu closed
  // within 150 ms of a hover; after a test's teardown it threw).
  useEffect(
    () => () => {
      if (hoverTimeoutRef.current) clearTimeout(hoverTimeoutRef.current);
    },
    []
  );

  const isSubmenuOpen =
    option.submenu &&
    activeSubmenu &&
    activeSubmenu.length >= path.length &&
    path.every((val, i) => val === activeSubmenu[i]);

  const handleMouseEnter = () => {
    cancelCloseSubmenu();
    // The pointer and the arrow keys move one highlight.
    if (!option.disabled) itemRef.current?.focus({ preventScroll: true });
    hoverTimeoutRef.current = setTimeout(() => {
      if (option.disabled) {
        const parentPath = path.slice(0, -1);
        openSubmenu(parentPath.length > 0 ? parentPath : null);
        return;
      }
      if (option.submenu) {
        openSubmenu(path);
      } else {
        const parentPath = path.slice(0, -1);
        openSubmenu(parentPath.length > 0 ? parentPath : null);
      }
    }, 150);
  };

  const handleMouseLeave = () => {
    if (hoverTimeoutRef.current) {
      clearTimeout(hoverTimeoutRef.current);
    }

    if (option.submenu && !option.disabled && !hasInteractiveSubmenu) {
      closeSubmenu(path);
    }
  };

  if (option.type === OPTION_SEPARATOR) {
    return <div className="rawtonic-menu-separator" />;
  }
  if (option.type === OPTION_HEADER) {
    return (
      <div className="rawtonic-menu-heading rawtonic-menu-group" role="presentation">
        {option.label}
      </div>
    );
  }

  return (
    <div onMouseEnter={handleMouseEnter} onMouseLeave={handleMouseLeave} className="relative">
      <button
        className={clsx(
          'rawtonic-menu-item',
          option.isDestructive && 'is-destructive',
          option.disabled && 'is-disabled'
        )}
        disabled={option.disabled}
        onClick={() => {
          if (!option.disabled && !option.submenu && option.onClick) {
            option.onClick();
            hideContextMenu();
          }
          // A click opens a submenu too (as in macOS menus), not only hovering.
          if (!option.disabled && option.submenu) {
            cancelCloseSubmenu();
            openSubmenu(path);
          }
        }}
        onMouseDown={(event) => {
          if (event.button !== 2) return;
          event.preventDefault();
          event.stopPropagation();
          if (!option.disabled && !option.submenu && option.onRightClick) {
            option.onRightClick();
            hideContextMenu();
          }
        }}
        ref={itemRef}
        role={option.checked === undefined ? 'menuitem' : 'menuitemradio'}
        aria-checked={option.checked}
        aria-haspopup={option.submenu ? 'menu' : undefined}
        aria-expanded={option.submenu ? !!isSubmenuOpen : undefined}
        aria-keyshortcuts={option.shortcut ?? undefined}
        data-menu-item={path.join('.')}
        tabIndex={-1}
      >
        <div className="flex items-center gap-2.5 min-w-0">
          {option.checked !== undefined &&
            (option.checked ? (
              <Check size={14} className="shrink-0" aria-hidden="true" />
            ) : (
              <span className="w-3.5 shrink-0" aria-hidden="true" />
            ))}
          {option.color && (
            <div className="w-2.5 h-2.5 rounded-full shrink-0" style={{ backgroundColor: option.color }}></div>
          )}
          {option.icon && <option.icon size={14} className="shrink-0" />}
          <span
            className="truncate"
            // Only a label the menu's widest size still cuts off gets its
            // full text as a tooltip.
            onMouseEnter={(e) => {
              const label = e.currentTarget;
              label.title = label.scrollWidth > label.clientWidth ? (option.label ?? '') : '';
            }}
          >
            {option.label}
          </span>
        </div>
        {option.shortcut && !option.submenu && (
          <span className="rawtonic-menu-shortcut" aria-hidden="true">
            {option.shortcut}
          </span>
        )}
        {option.submenu && <ChevronRight size={13} />}
      </button>

      <AnimatePresence>
        {isSubmenuOpen && (
          <SubMenu
            cancelCloseSubmenu={cancelCloseSubmenu}
            closeSubmenu={closeSubmenu}
            hideContextMenu={hideContextMenu}
            options={option.submenu!}
            parentRef={itemRef}
            parentPath={path}
          />
        )}
      </AnimatePresence>
    </div>
  );
}

function ContextMenu() {
  const { menuState, hideContextMenu, menuRef, menuId } = useContextMenu();
  const { isVisible, x, y, options, requested } = menuState;

  // The estimate in showContextMenu assumes tall rows and a fixed width (the
  // menu is as wide as its longest entry, translations differ); once it is
  // laid out, place it by its real size: at the requested point when it
  // fits, otherwise moved up or left just enough to stay inside the window.
  useLayoutEffect(() => {
    const el = menuRef.current;
    if (!isVisible || !el || !requested || !el.offsetHeight) return;
    const top = Math.max(5, Math.min(requested.y, window.innerHeight - el.offsetHeight - 10));
    const left = Math.max(5, Math.min(requested.x, window.innerWidth - el.offsetWidth - 10));
    el.style.top = `${top}px`;
    el.style.left = `${left}px`;
  }, [isVisible, menuId, requested, menuRef]);
  // Opened from the keyboard: its first row takes the focus.
  useLayoutEffect(() => {
    if (isVisible) focusFirstRow('');
  }, [isVisible, menuId]);

  return createPortal(
    <AnimatePresence>
      {isVisible && (
        <motion.div
          animate={{ opacity: 1, scale: 1 }}
          className="fixed z-50"
          exit={{ opacity: 0, scale: 0.95 }}
          initial={{ opacity: 0, scale: 0.95 }}
          key={menuId}
          onContextMenu={(e: React.MouseEvent) => e.preventDefault()}
          ref={menuRef}
          style={{ top: y, left: x }}
          transition={{ duration: 0.1, ease: 'easeOut' }}
        >
          <div className="rawtonic-menu rawtonic-menu-list min-w-60" data-menu-path="" role="menu">
            {options.map((option: Option, index: number) => (
              <MenuItem hideContextMenu={hideContextMenu} key={index} option={option} path={[index]} />
            ))}
          </div>
        </motion.div>
      )}
    </AnimatePresence>,
    overlayHost()
  );
}

export function ContextMenuProvider({ children }: ContextMenuProviderProps) {
  const [menuState, setMenuState] = useState<MenuState>({ isVisible: false, x: 0, y: 0, options: [] });
  const [activeSubmenu, setActiveSubmenu] = useState<number[] | null>(null);
  const [menuId, setMenuId] = useState<number>(0);
  const menuRef = useRef<HTMLDivElement>(null);
  const submenuTimeoutRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(submenuTimeoutRef.current), []);

  /** What had the focus before the menu: it gets it back when the menu closes. */
  const focusBefore = useRef<Element | null>(null);
  const showContextMenu = useCallback(
    (x: number, y: number, options: Array<Option>, open?: { focusFirst?: boolean }) => {
      const active = document.activeElement;
      if (!active?.closest('[role="menu"]')) focusBefore.current = active;
      focusFirstIn = open?.focusFirst ? '' : null;
      const menuWidth = 256;
      const menuHeight =
        options.reduce(
          (acc: number, opt: Option) =>
            acc + (opt.type === OPTION_SEPARATOR ? 9 : opt.type === OPTION_HEADER ? 22 : 40),
          0
        ) + 16;
      const adjustedX = x + menuWidth > window.innerWidth ? window.innerWidth - menuWidth - 10 : x;
      // Never above the window: a long menu's first entries must stay reachable.
      const adjustedY = Math.max(5, y + menuHeight > window.innerHeight ? window.innerHeight - menuHeight - 10 : y);

      setMenuState({ isVisible: true, x: adjustedX, y: adjustedY, requested: { x, y }, options });
      setMenuId((id) => id + 1);
      setActiveSubmenu(null);
    },
    []
  );

  const hideContextMenu = useCallback(() => {
    setMenuState((prev: MenuState) => ({ ...prev, isVisible: false }));
    setActiveSubmenu(null);
    focusFirstIn = null;
    // The focus goes back where it was (a dialog the choice opened keeps it).
    const active = document.activeElement;
    if (active?.closest('[role="menu"]') || active === document.body) {
      const before = focusBefore.current;
      if (before instanceof HTMLElement && before.isConnected) before.focus({ preventScroll: true });
      else (active as HTMLElement | null)?.blur?.();
    }
    focusBefore.current = null;
  }, []);

  const openSubmenu = useCallback((path: number[] | null) => {
    clearTimeout(submenuTimeoutRef.current);
    setActiveSubmenu(path);
  }, []);

  const closeSubmenu = useCallback((path: number[]) => {
    submenuTimeoutRef.current = setTimeout(() => {
      setActiveSubmenu((currentActivePath) => {
        if (currentActivePath && currentActivePath.join('-').startsWith(path.join('-'))) {
          const parentPath = path.slice(0, -1);
          return parentPath.length > 0 ? parentPath : null;
        }
        return currentActivePath;
      });
    }, 200);
  }, []);

  const cancelCloseSubmenu = useCallback(() => {
    clearTimeout(submenuTimeoutRef.current);
  }, []);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      const menuElements = document.querySelectorAll('[role="menu"]');
      let isClickInside = false;
      menuElements.forEach((menuEl) => {
        if (menuEl.contains(event.target as Node | null)) {
          isClickInside = true;
        }
      });

      if (!isClickInside) {
        hideContextMenu();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      // A field inside a menu (the keyword list's search) keeps its keys.
      if (target?.closest('[role="menu"]') && target.closest('input,textarea,select,[contenteditable="true"]')) return;
      if (event.metaKey || event.ctrlKey) {
        // ⌘/Ctrl combos (undo, the menu bar's keys) close the menu and go on.
        hideContextMenu();
        return;
      }
      const take = () => {
        event.preventDefault();
        event.stopImmediatePropagation();
      };
      const focused = document.activeElement instanceof HTMLButtonElement ? document.activeElement : null;
      const row = focused?.closest('[role="menu"]') ? focused : null;
      const panel = row?.closest<HTMLElement>('[role="menu"]') ?? panelOf('');
      const panelPath = panel?.dataset.menuPath ?? '';
      const rows = panel ? menuRows(panel) : [];
      const index = row ? rows.indexOf(row) : -1;
      const focusRow = (next: number) => rows[(next + rows.length) % rows.length]?.focus({ preventScroll: true });
      const openFrom = (button: HTMLButtonElement) => {
        const path = (button.dataset.menuItem ?? '').split('.').filter(Boolean).map(Number);
        focusFirstIn = path.join('.');
        openSubmenu(path);
        // Already open (hovered): focus its first row now.
        focusFirstRow(path.join('.'));
      };
      const closeLevel = () => {
        if (!panelPath) {
          hideContextMenu();
          return;
        }
        const path = panelPath.split('.').map(Number);
        openSubmenu(path.length > 1 ? path.slice(0, -1) : null);
        document
          .querySelector<HTMLButtonElement>(`button[data-menu-item="${panelPath}"]`)
          ?.focus({ preventScroll: true });
      };
      switch (event.key) {
        case 'Escape':
          take();
          // In a submenu Escape closes that level (Windows); at the top, the menu.
          if (row) closeLevel();
          else hideContextMenu();
          return;
        case 'ArrowDown':
          take();
          focusRow(index + 1);
          return;
        case 'ArrowUp':
          take();
          focusRow(index < 0 ? -1 : index - 1);
          return;
        case 'Home':
          take();
          focusRow(0);
          return;
        case 'End':
          take();
          focusRow(-1);
          return;
        case 'ArrowRight':
          take();
          if (row?.getAttribute('aria-haspopup')) openFrom(row);
          return;
        case 'ArrowLeft':
          take();
          if (panelPath) closeLevel();
          return;
        case 'Enter':
        case ' ':
          take();
          if (!row) return;
          if (row.getAttribute('aria-haspopup')) openFrom(row);
          else row.click();
          return;
        case 'Tab':
          take();
          return;
      }
      if (event.key.length === 1 && !event.altKey && rows.length) {
        // Type to go: the next row whose name starts with the letter.
        take();
        const letter = event.key.toLocaleLowerCase();
        for (let step = 1; step <= rows.length; step++) {
          const candidate = rows[(index + step) % rows.length];
          if (candidate.textContent?.trim().toLocaleLowerCase().startsWith(letter)) {
            candidate.focus({ preventScroll: true });
            return;
          }
        }
        return;
      }
      // Any other key stays with the menu, away from the app's shortcuts
      // (F10 and Alt are the menu opener's).
      if (event.key !== 'F10' && event.key !== 'Alt') take();
    };

    if (menuState.isVisible) {
      document.addEventListener('mousedown', handleClickOutside);
      window.addEventListener('scroll', hideContextMenu, true);
      menuKeys = handleKeyDown;
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      window.removeEventListener('scroll', hideContextMenu, true);
      if (menuKeys === handleKeyDown) menuKeys = null;
    };
  }, [menuState.isVisible, hideContextMenu, openSubmenu]);

  const value: ContextMenuContextValue = {
    activeSubmenu,
    cancelCloseSubmenu,
    closeSubmenu,
    hideContextMenu,
    menuId,
    menuRef,
    menuState,
    openSubmenu,
    showContextMenu
  };

  return (
    <ContextMenuContext.Provider value={value}>
      {children}
      <ContextMenu />
    </ContextMenuContext.Provider>
  );
}

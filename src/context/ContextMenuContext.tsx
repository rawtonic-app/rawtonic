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
  RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { Check, ChevronRight } from 'lucide-react';
import { Option, OPTION_HEADER, OPTION_SEPARATOR } from '../components/ui/AppProperties';
import clsx from 'clsx';

// A modal <dialog> sits in the top layer and makes the rest of the document
// inert, so a menu opened from inside it must render inside it as well.
function menuHost(): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>('dialog[open]')).at(-1) ?? document.body;
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
  showContextMenu(x: number, y: number, options: Array<Option>): void;
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
  }, [parentRef, options, menuHeight]);

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

  return createPortal(menuMarkup, menuHost());
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
    [],
  );

  const isSubmenuOpen =
    option.submenu &&
    activeSubmenu &&
    activeSubmenu.length >= path.length &&
    path.every((val, i) => val === activeSubmenu[i]);

  const handleMouseEnter = () => {
    cancelCloseSubmenu();
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
          option.disabled && 'is-disabled',
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
      >
        <div className="flex items-center gap-2.5 min-w-0">
          {option.checked !== undefined &&
            (option.checked ? (
              <Check size={14} className="shrink-0" aria-hidden="true" />
            ) : (
              <span className="w-3.5 shrink-0" aria-hidden="true" />
            ))}
          {option.color && <div className="w-2.5 h-2.5 rounded-full shrink-0" style={{ backgroundColor: option.color }}></div>}
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
          <div className="rawtonic-menu rawtonic-menu-list min-w-60" role="menu">
            {options.map((option: Option, index: number) => (
              <MenuItem hideContextMenu={hideContextMenu} key={index} option={option} path={[index]} />
            ))}
          </div>
        </motion.div>
      )}
    </AnimatePresence>,
    menuHost(),
  );
}

export function ContextMenuProvider({ children }: ContextMenuProviderProps) {
  const [menuState, setMenuState] = useState<MenuState>({ isVisible: false, x: 0, y: 0, options: [] });
  const [activeSubmenu, setActiveSubmenu] = useState<number[] | null>(null);
  const [menuId, setMenuId] = useState<number>(0);
  const menuRef = useRef<HTMLDivElement>(null);
  const submenuTimeoutRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(submenuTimeoutRef.current), []);

  const showContextMenu = useCallback((x: number, y: number, options: Array<Option>) => {
    const menuWidth = 256;
    const menuHeight =
      options.reduce(
        (acc: number, opt: Option) =>
          acc + (opt.type === OPTION_SEPARATOR ? 9 : opt.type === OPTION_HEADER ? 22 : 40),
        0,
      ) + 16;
    const adjustedX = x + menuWidth > window.innerWidth ? window.innerWidth - menuWidth - 10 : x;
    // Never above the window: a long menu's first entries must stay reachable.
    const adjustedY = Math.max(5, y + menuHeight > window.innerHeight ? window.innerHeight - menuHeight - 10 : y);

    setMenuState({ isVisible: true, x: adjustedX, y: adjustedY, requested: { x, y }, options });
    setMenuId((id) => id + 1);
    setActiveSubmenu(null);
  }, []);

  const hideContextMenu = useCallback(() => {
    setMenuState((prev: MenuState) => ({ ...prev, isVisible: false }));
    setActiveSubmenu(null);
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
      if (event.key === 'Escape') {
        // Escape only closes the menu; the app's shortcuts (on window) must not
        // also leave Develop or the crop tool.
        event.stopPropagation();
        hideContextMenu();
      }
    };

    if (menuState.isVisible) {
      document.addEventListener('mousedown', handleClickOutside);
      window.addEventListener('scroll', hideContextMenu, true);
      document.addEventListener('keydown', handleKeyDown);
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      window.removeEventListener('scroll', hideContextMenu, true);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [menuState.isVisible, hideContextMenu]);

  const value: ContextMenuContextValue = {
    activeSubmenu,
    cancelCloseSubmenu,
    closeSubmenu,
    hideContextMenu,
    menuId,
    menuRef,
    menuState,
    openSubmenu,
    showContextMenu,
  };

  return (
    <ContextMenuContext.Provider value={value}>
      {children}
      <ContextMenu />
    </ContextMenuContext.Provider>
  );
}

import { useState, useEffect, useRef } from 'react';
import { toast } from 'react-toastify';
import { X, Plus } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { AppSettings, Invokes } from '../components/ui/AppProperties';
import Text from '../components/ui/Text';
import { TextVariants } from '../types/typography';
import { useLibraryStore } from '../store/useLibraryStore';
import { expandGroupedPaths } from '../utils/imageGrouping';
import { invokeInOrder } from '../utils/catalogWrites';
import { saveFailure } from '../hooks/useLibraryActions';

interface TaggingSubMenuProps {
  paths: string[];
  initialTags: { tag: string; isUser: boolean }[];
  /** `newTags` for `paths` and their grouped variants: every planned photo
   * saved. */
  /** The one stored tag (`user:client`, `dog`) a write saved for exactly
   * `savedPaths`, when the rest failed: only that tag changes on those
   * photos, every other keyword they have stays as the library saved it. */
  onTagSaved: (savedPaths: string[], tag: string, change: 'add' | 'remove') => void;
  appSettings: AppSettings | null;
  hideContextMenu: () => void;
}

const USER_TAG_PREFIX = 'user:';

const tagVariants = {
  visible: { opacity: 1, scale: 1, transition: { type: 'spring' as const, stiffness: 500, damping: 30 } },
  exit: { opacity: 0, scale: 0.8, transition: { duration: 0.15 } },
};

export default function TaggingSubMenu({
  paths,
  initialTags,
  onTagSaved,
  appSettings,
  hideContextMenu,
}: TaggingSubMenuProps) {
  const { t } = useTranslation();
  const [tags, setTags] = useState<{ tag: string; isUser: boolean }[]>(initialTags);
  const [inputValue, setInputValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setTags(initialTags);
  }, [initialTags]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const getPathsToUpdate = () =>
    expandGroupedPaths(useLibraryStore.getState().imageList, paths, appSettings?.grouping ?? 'off');

  /** Adds the tag to every photo (and its variants), keeping the menu's
   * other tags; see `applyTags`. */
  const handleAddTag = async (tagToAdd: string) => {
    const newTagValue = tagToAdd.trim().toLowerCase();
    if (newTagValue && !tags.some((t) => t.tag === newTagValue)) {
      const added = await applyTags(`${USER_TAG_PREFIX}${newTagValue}`, true, (kept) =>
        [...kept, { tag: newTagValue, isUser: true }].sort((a, b) => a.tag.localeCompare(b.tag)),
      );
      if (added) setInputValue('');
    }
  };

  /** Removes the tag; see `applyTags`. */
  const handleRemoveTag = async (tagToRemove: { tag: string; isUser: boolean }) => {
    await applyTags(tagToRemove.isUser ? `${USER_TAG_PREFIX}${tagToRemove.tag}` : tagToRemove.tag, false, (kept) =>
      kept.filter((t) => t.tag !== tagToRemove.tag),
    );
  };

  /**
   * Writes `tag` to the photos (and their variants), then shows the result.
   * `next` makes the menu's tags after an attempt from its tags without the
   * written one: adding the tag leaves every other tag as it was, removing
   * takes it out.
   *
   * The photos that saved show the one tag written and keep their other
   * keywords (`onTagSaved`, exactly them). Saved everywhere: the menu takes
   * the new tags. Saved for some: the menu keeps its own tags, and adding
   * the tag again retries it for every photo (adding is idempotent). The XMP status follows the photos that failed (photoWrites).
   */
  const applyTags = async (
    tag: string,
    adds: boolean,
    next: (kept: { tag: string; isUser: boolean }[]) => { tag: string; isUser: boolean }[],
  ) => {
    const pathsToUpdate = getPathsToUpdate();
    try {
      await invokeInOrder(
        adds ? Invokes.AddTagForPaths : Invokes.RemoveTagForPaths,
        { paths: pathsToUpdate, tag },
        'keyword',
      );
    } catch (err) {
      const { message, failed } = saveFailure(err, pathsToUpdate);
      const saved = pathsToUpdate.filter((path) => !failed.includes(path));
      if (saved.length > 0) onTagSaved(saved, tag, adds ? 'add' : 'remove');
      toast.error(`${adds ? 'Failed to add tag' : 'Failed to remove tag'}: ${message}`);
      return false;
    }
    // Every photo saved: each shows the one tag written and keeps its other
    // keywords (the menu lists only the tags all photos share).
    onTagSaved(pathsToUpdate, tag, adds ? 'add' : 'remove');
    setTags(next(tags));
    return true;
  };

  const handleInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      handleAddTag(inputValue);
    }
    if (e.key === 'Escape') {
      hideContextMenu();
    }
  };

  const shortcuts = appSettings?.taggingShortcuts || [];

  return (
    <div
      className="bg-surface/95 p-2 w-64 text-text-primary rounded-lg"
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
    >
      <div className="mb-2">
        <div className="flex flex-wrap gap-1 p-1 bg-surface rounded-md min-h-[32px] items-center">
          <AnimatePresence>
            {tags.length > 0 ? (
              tags.map((tagItem) => (
                <motion.div
                  key={tagItem.tag}
                  layout
                  variants={tagVariants}
                  initial={false}
                  animate="visible"
                  exit="exit"
                  onClick={() => handleRemoveTag(tagItem)}
                  data-tooltip={t('menus.tagging.removeTooltip', { tag: tagItem.tag })}
                  className="flex items-center gap-1 bg-bg-primary text-text-primary text-xs font-medium px-2 py-1 rounded-sm group cursor-pointer"
                >
                  <span>{tagItem.tag}</span>
                  <span className="rounded-full group-hover:bg-black/20 p-0.5 transition-colors">
                    <X size={12} />
                  </span>
                </motion.div>
              ))
            ) : (
              <motion.span
                key="no-tags-placeholder"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.2 }}
                className="p-1 select-none"
              >
                <Text variant={TextVariants.small} className="italic">
                  {t('menus.tagging.noTags')}
                </Text>
              </motion.span>
            )}
          </AnimatePresence>
        </div>
      </div>

      <div className="relative mb-2">
        <input
          ref={inputRef}
          type="text"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={handleInputKeyDown}
          placeholder={t('menus.tagging.placeholder')}
          className="w-full bg-surface border border-border-color rounded-md pl-2 pr-8 py-1.5 text-sm focus:outline-hidden"
        />
        <button
          onClick={() => handleAddTag(inputValue)}
          className="absolute right-1 top-1/2 -translate-y-1/2 p-1 rounded-full text-text-secondary hover:text-text-primary hover:bg-surface"
          data-tooltip={t('menus.tagging.addTagTooltip')}
        >
          <Plus size={16} />
        </button>
      </div>

      {shortcuts.length > 0 && (
        <div>
          <p className="text-xs font-semibold text-text-primary px-1 py-1">{t('menus.tagging.shortcutsHeading')}</p>
          <div className="flex flex-wrap gap-1">
            {shortcuts.map((shortcut: string) => (
              <button
                key={shortcut}
                onClick={() => handleAddTag(shortcut)}
                className="bg-surface text-text-secondary hover:bg-card-active hover:text-text-primary text-xs font-medium px-2 py-1 rounded-sm"
              >
                {shortcut}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

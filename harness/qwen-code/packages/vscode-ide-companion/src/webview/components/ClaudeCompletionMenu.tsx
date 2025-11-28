/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useEffect, useRef, useState } from 'react';
import './ClaudeCompletionMenu.css';
import type { CompletionItem } from './CompletionMenu.js';

interface ClaudeCompletionMenuProps {
  items: CompletionItem[];
  onSelect: (item: CompletionItem) => void;
  onClose: () => void;
  title?: string;
  selectedIndex?: number;
}

/**
 * Claude Code-like anchored dropdown rendered above the input field.
 * Keyboard: Up/Down to move, Enter to select, Esc to close.
 */
export const ClaudeCompletionMenu: React.FC<ClaudeCompletionMenuProps> = ({
  items,
  onSelect,
  onClose,
  title,
  selectedIndex = 0,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState(selectedIndex);

  useEffect(() => setSelected(selectedIndex), [selectedIndex]);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        onClose();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      switch (event.key) {
        case 'ArrowDown':
          event.preventDefault();
          setSelected((prev) => Math.min(prev + 1, items.length - 1));
          break;
        case 'ArrowUp':
          event.preventDefault();
          setSelected((prev) => Math.max(prev - 1, 0));
          break;
        case 'Enter':
          event.preventDefault();
          if (items[selected]) {
            onSelect(items[selected]);
          }
          break;
        case 'Escape':
          event.preventDefault();
          onClose();
          break;
        default:
          break;
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [items, selected, onSelect, onClose]);

  useEffect(() => {
    const selectedEl = containerRef.current?.querySelector(
      `[data-index="${selected}"]`,
    );
    if (selectedEl) {
      selectedEl.scrollIntoView({ block: 'nearest' });
    }
  }, [selected]);

  if (!items.length) {
    return null;
  }

  return (
    <div ref={containerRef} className="hi" role="menu">
      <div className="spacer-4px" />
      <div className="xi">
        {title && <div className="vi">{title}</div>}
        {items.map((item, index) => {
          const selectedCls = index === selected ? 'jo' : '';
          return (
            <div
              key={item.id}
              data-index={index}
              className={`wi ${selectedCls}`}
              onClick={() => onSelect(item)}
              onMouseEnter={() => setSelected(index)}
              role="menuitem"
            >
              <div className="ki">
                {item.icon && <span className="Ii">{item.icon}</span>}
                <span className="Lo">{item.label}</span>
                {item.description && (
                  <span className="Mo" title={item.description}>
                    {item.description}
                  </span>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
};

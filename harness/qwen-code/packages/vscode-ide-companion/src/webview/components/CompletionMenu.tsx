/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useEffect, useRef, useState } from 'react';
import './CompletionMenu.css';

export interface CompletionItem {
  id: string;
  label: string;
  description?: string;
  icon?: React.ReactNode;
  type: 'file' | 'symbol' | 'command' | 'variable';
  value?: unknown;
}

interface CompletionMenuProps {
  items: CompletionItem[];
  position: { top: number; left: number };
  onSelect: (item: CompletionItem) => void;
  onClose: () => void;
  selectedIndex?: number;
}

/**
 * Completion menu for @ and / triggers
 * Based on vscode-copilot-chat's AttachContextAction
 */
export const CompletionMenu: React.FC<CompletionMenuProps> = ({
  items,
  position,
  onSelect,
  onClose,
  selectedIndex = 0,
}) => {
  const menuRef = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState(selectedIndex);

  useEffect(() => {
    setSelected(selectedIndex);
  }, [selectedIndex]);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
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

  // Scroll selected item into view
  useEffect(() => {
    const selectedElement = menuRef.current?.querySelector(
      `[data-index="${selected}"]`,
    );
    if (selectedElement) {
      selectedElement.scrollIntoView({ block: 'nearest' });
    }
  }, [selected]);

  if (items.length === 0) {
    return null;
  }

  return (
    <div
      ref={menuRef}
      className="completion-menu"
      style={{
        top: `${position.top}px`,
        left: `${position.left}px`,
      }}
    >
      <div className="completion-menu-items">
        {items.map((item, index) => (
          <div
            key={item.id}
            data-index={index}
            className={`completion-menu-item ${index === selected ? 'selected' : ''}`}
            onClick={() => onSelect(item)}
            onMouseEnter={() => setSelected(index)}
          >
            {item.icon && (
              <div className="completion-item-icon">{item.icon}</div>
            )}
            <div className="completion-item-content">
              <div className="completion-item-label">{item.label}</div>
              {item.description && (
                <div className="completion-item-description">
                  {item.description}
                </div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};

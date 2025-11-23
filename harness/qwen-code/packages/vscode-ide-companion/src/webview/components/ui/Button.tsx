/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Button component using Tailwind CSS
 * This is an example of how to create new components using Tailwind
 * while maintaining compatibility with existing CSS-based components
 */

import type React from 'react';

interface ButtonProps {
  /**
   * Button variant style
   */
  variant?: 'primary' | 'secondary' | 'ghost' | 'icon';

  /**
   * Button size
   */
  size?: 'sm' | 'md' | 'lg';

  /**
   * Button contents
   */
  children: React.ReactNode;

  /**
   * Optional click handler
   */
  onClick?: () => void;

  /**
   * Disable button
   */
  disabled?: boolean;

  /**
   * Additional class names
   */
  className?: string;
}

/**
 * Primary UI component for user interaction
 */
export const Button: React.FC<ButtonProps> = ({
  variant = 'primary',
  size = 'md',
  children,
  onClick,
  disabled = false,
  className = '',
}) => {
  // Base classes that apply to all buttons
  const baseClasses = "inline-flex items-center justify-center rounded-md font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:opacity-50 disabled:pointer-events-none";

  // Variant-specific classes
  const variantClasses = {
    primary: "bg-qwen-orange text-qwen-ivory hover:bg-qwen-clay-orange shadow-sm",
    secondary: "bg-gray-100 text-gray-900 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-100 dark:hover:bg-gray-700",
    ghost: "hover:bg-gray-100 dark:hover:bg-gray-800",
    icon: "hover:bg-gray-100 dark:hover:bg-gray-800 p-1"
  };

  // Size-specific classes
  const sizeClasses = {
    sm: "h-8 px-3 text-xs",
    md: "h-10 px-4 py-2 text-sm",
    lg: "h-12 px-6 text-base"
  };

  // Combine all classes
  const classes = `${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]} ${className}`;

  return (
    <button
      type="button"
      className={classes}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </button>
  );
};
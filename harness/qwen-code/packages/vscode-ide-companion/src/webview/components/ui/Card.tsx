/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Card component using Tailwind CSS
 * This demonstrates how to create new components with Tailwind
 * while maintaining compatibility with existing components
 */

import type React from 'react';

interface CardProps {
  /**
   * Card contents
   */
  children: React.ReactNode;

  /**
   * Additional class names
   */
  className?: string;
}

interface CardHeaderProps {
  /**
   * Card header contents
   */
  children: React.ReactNode;

  /**
   * Additional class names
   */
  className?: string;
}

interface CardContentProps {
  /**
   * Card content contents
   */
  children: React.ReactNode;

  /**
   * Additional class names
   */
  className?: string;
}

interface CardFooterProps {
  /**
   * Card footer contents
   */
  children: React.ReactNode;

  /**
   * Additional class names
   */
  className?: string;
}

/**
 * Card container component
 */
const Card: React.FC<CardProps> & {
  Header: React.FC<CardHeaderProps>;
  Content: React.FC<CardContentProps>;
  Footer: React.FC<CardFooterProps>;
} = ({
  children,
  className = '',
}) => {
  return (
    <div className={`rounded-lg border bg-card text-card-foreground shadow-sm ${className}`}>
      {children}
    </div>
  );
};

/**
 * Card header component
 */
const CardHeader: React.FC<CardHeaderProps> = ({
  children,
  className = '',
}) => {
  return (
    <div className={`flex flex-col space-y-1.5 p-6 ${className}`}>
      {children}
    </div>
  );
};

/**
 * Card content component
 */
const CardContent: React.FC<CardContentProps> = ({
  children,
  className = '',
}) => {
  return (
    <div className={`p-6 pt-0 ${className}`}>
      {children}
    </div>
  );
};

/**
 * Card footer component
 */
const CardFooter: React.FC<CardFooterProps> = ({
  children,
  className = '',
}) => {
  return (
    <div className={`flex items-center p-6 pt-0 ${className}`}>
      {children}
    </div>
  );
};

// Compose the Card component with its subcomponents
Card.Header = CardHeader;
Card.Content = CardContent;
Card.Footer = CardFooter;

export { Card };
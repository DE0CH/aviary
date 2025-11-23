/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Demo page showing how to use Tailwind components alongside existing CSS
 * This demonstrates the progressive adoption approach
 */

import type React from 'react';
import { Button } from './ui/Button';
import { Card } from './ui/Card';

/**
 * Demo component showing how to use Tailwind components
 * alongside existing CSS-based components
 */
export const TailwindDemo: React.FC = () => {
  return (
    <div className="p-4 max-w-2xl mx-auto">
      <h1 className="text-2xl font-bold mb-6 text-gray-900 dark:text-white">
        Tailwind CSS Progressive Adoption Demo
      </h1>

      <p className="mb-6 text-gray-700 dark:text-gray-300">
        This page demonstrates how to gradually adopt Tailwind CSS in an existing project
        while maintaining compatibility with existing CSS styles.
      </p>

      {/* Example 1: Using new Tailwind components */}
      <Card className="mb-6">
        <Card.Header>
          <h2 className="text-xl font-semibold">New Components with Tailwind</h2>
        </Card.Header>
        <Card.Content>
          <p className="mb-4 text-gray-600 dark:text-gray-400">
            These are new components built with Tailwind CSS:
          </p>
          <div className="flex flex-wrap gap-3">
            <Button variant="primary">Primary Button</Button>
            <Button variant="secondary">Secondary Button</Button>
            <Button variant="ghost">Ghost Button</Button>
          </div>
        </Card.Content>
      </Card>

      {/* Example 2: Hybrid approach - mixing Tailwind with existing styles */}
      <div className="permission-request-card mb-6 p-4 rounded-lg border border-gray-200 dark:border-gray-700">
        <h2 className="text-xl font-semibold mb-4">Hybrid Approach</h2>
        <p className="mb-4 text-gray-600 dark:text-gray-400">
          This card uses existing CSS classes but enhances them with Tailwind utilities:
        </p>
        <div className="flex flex-wrap gap-3">
          <button className="permission-confirm-button bg-qwen-orange hover:bg-qwen-clay-orange text-qwen-ivory px-4 py-2 rounded">
            Enhanced Button
          </button>
          <button className="permission-option bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 px-4 py-2 rounded">
            Hybrid Option
          </button>
        </div>
      </div>

      {/* Example 3: Utility-first approach */}
      <div className="mb-6 p-6 bg-white dark:bg-gray-800 rounded-lg shadow-md border border-gray-200 dark:border-gray-700">
        <h2 className="text-xl font-semibold mb-4 text-gray-900 dark:text-white">Utility-First Approach</h2>
        <p className="mb-4 text-gray-600 dark:text-gray-400">
          This section uses Tailwind's utility-first approach:
        </p>
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="p-4 bg-blue-50 dark:bg-blue-900/20 rounded-lg border border-blue-200 dark:border-blue-800">
            <h3 className="font-medium text-blue-800 dark:text-blue-200">Feature 1</h3>
            <p className="mt-2 text-sm text-blue-600 dark:text-blue-300">
              Description of feature 1
            </p>
          </div>
          <div className="p-4 bg-green-50 dark:bg-green-900/20 rounded-lg border border-green-200 dark:border-green-800">
            <h3 className="font-medium text-green-800 dark:text-green-200">Feature 2</h3>
            <p className="mt-2 text-sm text-green-600 dark:text-green-300">
              Description of feature 2
            </p>
          </div>
          <div className="p-4 bg-purple-50 dark:bg-purple-900/20 rounded-lg border border-purple-200 dark:border-purple-800">
            <h3 className="font-medium text-purple-800 dark:text-purple-200">Feature 3</h3>
            <p className="mt-2 text-sm text-purple-600 dark:text-purple-300">
              Description of feature 3
            </p>
          </div>
        </div>
      </div>

      {/* Example 4: Responsive design */}
      <div className="p-6 bg-gradient-to-r from-qwen-orange to-qwen-clay-orange rounded-lg text-white">
        <h2 className="text-xl font-semibold mb-2">Responsive Design</h2>
        <p className="mb-4 opacity-90">
          Tailwind makes responsive design easy with breakpoint prefixes:
        </p>
        <div className="flex flex-col sm:flex-row gap-4">
          <div className="flex-1 p-4 bg-black/10 rounded">
            <p className="text-center">Column 1</p>
          </div>
          <div className="flex-1 p-4 bg-black/10 rounded">
            <p className="text-center">Column 2</p>
          </div>
          <div className="flex-1 p-4 bg-black/10 rounded">
            <p className="text-center">Column 3</p>
          </div>
        </div>
      </div>
    </div>
  );
};
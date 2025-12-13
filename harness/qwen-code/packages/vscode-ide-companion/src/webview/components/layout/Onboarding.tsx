/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { generateIconUrl } from '../../utils/resourceUrl.js';

interface OnboardingPageProps {
  onLogin: () => void;
}

export const Onboarding: React.FC<OnboardingPageProps> = ({ onLogin }) => {
  const iconUri = generateIconUrl('icon.png');

  return (
    <div className="flex flex-col items-center justify-center h-full p-5 md:p-10">
      <div className="flex flex-col items-center gap-8 w-full max-w-md">
        <div className="flex flex-col items-center gap-6">
          {/* Application icon container with brand logo and decorative close icon */}
          <div className="relative">
            <img
              src={iconUri}
              alt="Qwen Code Logo"
              className="w-[80px] h-[80px] object-contain"
            />
            {/* Decorative close icon for enhanced visual effect */}
            <div className="absolute -top-2 -right-2 w-6 h-6 bg-[#4f46e5] rounded-full flex items-center justify-center">
              <svg width="12" height="12" viewBox="0 0 12 12" fill="white">
                <path
                  d="M2.5 1.5L9.5 8.5M9.5 1.5L2.5 8.5"
                  stroke="white"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                />
              </svg>
            </div>
          </div>

          {/* Text content area */}
          <div className="text-center">
            <h1 className="text-2xl font-bold text-app-primary-foreground mb-2">
              Welcome to Qwen Code
            </h1>
            <p className="text-app-secondary-foreground max-w-sm">
              Qwen Code helps you understand, navigate, and transform your
              codebase with AI assistance.
            </p>
          </div>

          <button
            onClick={onLogin}
            className="w-full px-4 py-3 bg-[#4f46e5] text-white font-medium rounded-lg shadow-sm"
          >
            Log in to Qwen Code
          </button>
        </div>
      </div>
    </div>
  );
};

import type React from 'react';
import { generateIconUrl } from '../../utils/resourceUrl.js';

interface OnboardingPageProps {
  onLogin: () => void;
}

export const OnboardingPage: React.FC<OnboardingPageProps> = ({ onLogin }) => {
  const iconUri = generateIconUrl('icon.png');

  return (
    <div className="flex flex-col items-center justify-center h-full p-5 md:p-10">
      <div className="flex flex-col items-center gap-8 w-full max-w-md">
        <div className="flex flex-col items-center gap-6">
          <div className="relative">
            <img
              src={iconUri}
              alt="Qwen Code Logo"
              className="w-[80px] h-[80px] object-contain"
            />
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

          <div className="text-center">
            <h1 className="text-2xl font-bold text-app-primary-foreground mb-2">
              Welcome to Qwen Code
            </h1>
            <p className="text-app-secondary-foreground max-w-sm">
              Qwen Code helps you understand, navigate, and transform your
              codebase with AI assistance.
            </p>
          </div>

          {/* <div className="flex flex-col gap-5 w-full">
            <div className="bg-app-secondary-background rounded-xl p-5 border border-app-primary-border-color shadow-sm">
              <h2 className="font-semibold text-app-primary-foreground mb-3">Get Started</h2>
              <ul className="space-y-2">
                <li className="flex items-start gap-2">
                  <div className="mt-1 w-1.5 h-1.5 rounded-full bg-[#4f46e5] flex-shrink-0"></div>
                  <span className="text-sm text-app-secondary-foreground">Understand complex codebases faster</span>
                </li>
                <li className="flex items-start gap-2">
                  <div className="mt-1 w-1.5 h-1.5 rounded-full bg-[#4f46e5] flex-shrink-0"></div>
                  <span className="text-sm text-app-secondary-foreground">Navigate with AI-powered suggestions</span>
                </li>
                <li className="flex items-start gap-2">
                  <div className="mt-1 w-1.5 h-1.5 rounded-full bg-[#4f46e5] flex-shrink-0"></div>
                  <span className="text-sm text-app-secondary-foreground">Transform code with confidence</span>
                </li>
              </ul>
            </div>

          </div> */}

          <button
            onClick={onLogin}
            className="w-full px-4 py-3 bg-[#4f46e5] text-white font-medium rounded-lg shadow-sm"
          >
            Log in to Qwen Code
          </button>

          {/* <div className="text-center">
            <p className="text-xs text-app-secondary-foreground">
              By logging in, you agree to the Terms of Service and Privacy
              Policy.
            </p>
          </div> */}
        </div>
      </div>
    </div>
  );
};

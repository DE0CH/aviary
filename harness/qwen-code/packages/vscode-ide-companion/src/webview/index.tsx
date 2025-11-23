/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import ReactDOM from 'react-dom/client';
import { App } from './App.js';
import './tailwind.css';
import './App.scss';
import './ClaudeCodeStyles.css';

const container = document.getElementById('root');
if (container) {
  const root = ReactDOM.createRoot(container);
  root.render(<App />);
}

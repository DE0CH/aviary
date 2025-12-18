# Qwen Code

<div align="center">

![Qwen Code Screenshot](./docs/assets/qwen-screenshot.png)

[![npm version](https://img.shields.io/npm/v/@qwen-code/qwen-code.svg)](https://www.npmjs.com/package/@qwen-code/qwen-code)
[![License](https://img.shields.io/github/license/QwenLM/qwen-code.svg)](./LICENSE)
[![Node.js Version](https://img.shields.io/badge/node-%3E%3D20.0.0-brightgreen.svg)](https://nodejs.org/)
[![Downloads](https://img.shields.io/npm/dm/@qwen-code/qwen-code.svg)](https://www.npmjs.com/package/@qwen-code/qwen-code)

**AI-powered command-line workflow tool for developers**

[Installation](#installation) • [Quick Start](#quick-start) • [Features](#key-features) • [Documentation](./docs/) • [Contributing](./CONTRIBUTING.md)

</div>

<div align="center">
  
  <a href="https://qwenlm.github.io/qwen-code-docs/de/">Deutsch</a> | 
  <a href="https://qwenlm.github.io/qwen-code-docs/fr">français</a> | 
  <a href="https://qwenlm.github.io/qwen-code-docs/ja/">日本語</a> | 
  <a href="https://qwenlm.github.io/qwen-code-docs/ru">Русский</a> | 
  <a href="https://qwenlm.github.io/qwen-code-docs/zh/">中文</a>
  
</div>

Qwen Code is a powerful command-line AI workflow tool adapted from [**Gemini CLI**](https://github.com/google-gemini/gemini-cli), specifically optimized for [Qwen3-Coder](https://github.com/QwenLM/Qwen3-Coder) models. It enhances your development workflow with advanced code understanding, automated tasks, and intelligent assistance.

![](https://gw.alicdn.com/imgextra/i1/O1CN01D2DviS1wwtEtMwIzJ_!!6000000006373-2-tps-1600-900.png)


## 📌 Why Qwen Code？

**🌱 Seamless Integration & Immediate Setup**
   - Free & Open Source: Completely free to use with generous daily quotas (up
     to 2,000 requests/day)
   - Quick Start in 30 Seconds: One-click authentication with **Qwen OAuth**, no
     complex configuration needed

**🧠 Advanced Code Intelligence**
  - Deep Code Understanding: Grasp complex codebases beyond traditional context
    limits, instantly understanding architecture, dependencies, and data flows
  - Multi-Language Support and Smart Refactoring: Optimize, debug, and refactor code with AI-powered insights that follow industry standards

**🔧 Comprehensive Development Assistance**
  - Error Debugging Made Easy: Paste error logs directly for instant root cause
    analysis and actionable solutions
  - Git Workflow Enhancement: Generate standardized commit messages, explain Git
    commands, and assist with code reviews
  - Documentation & Testing: Automatically generate comprehensive documentation,
    JSDoc comments, and unit tests with edge cases

**💼 Professional [VS Code Integration](https://qwenlm.github.io/qwen-code-docs/en/users/integration-vscode/)**
  - Sidebar Companion: Install the VS Code extension for seamless integration
    with native diffing, interactive chat, and file system operations
  - Context-Aware Assistance: Get AI-powered help without leaving your
    preferred development environment

**🌐 Flexible Authentication**
  - Multiple Free Tiers: Access regional free options (Mainland China:
    ModelScope, International: OpenRouter) with up to 2,000 free API calls per
    day
  - OpenAI-Compatible: Use existing API keys from various providers (Alibaba
    Cloud, OpenRouter, etc.)

>👉 Know more [workflows](https://qwenlm.github.io/qwen-code-docs/en/users/common-workflow/)
>
> 📦 The extension is currently in development. For installation, features, and development guide, see the [VS Code Extension README](./packages/vscode-ide-companion/README.md).


## ❓ How to use Qwen Code?

### Prerequisites

Ensure you have [Node.js version 20](https://nodejs.org/en/download) or higher installed.

```bash
curl -qL https://www.npmjs.com/install.sh | sh
```

### Install from npm

```bash
npm install -g @qwen-code/qwen-code@latest
```

## 🚀 Quick Start

```bash
# Start Qwen Code
qwen

# Example commands
> What does this project do?
> Explain this codebase structure
> Help me refactor this function
> Generate unit tests for this module
```

![](https://gw.alicdn.com/imgextra/i2/O1CN01bgnQbj1XcrWfFWeNc_!!6000000002945-1-tps-852-480.gif)


## Usage Examples

If you want to learn more about common workflows, click [Common Workflows](https://qwenlm.github.io/qwen-code-docs/en/users/common-workflow/) to view.

### 🔍 Explore Codebases

```bash
cd your-project/
qwen

# Architecture analysis
> Describe the main pieces of this system's architecture
> What are the key dependencies and how do they interact?
> Find all API endpoints and their authentication methods
```

### 💻 Code Development

```bash
# Refactoring
> Refactor this function to improve readability and performance
> Convert this class to use dependency injection
> Split this large module into smaller, focused components

# Code generation
> Create a REST API endpoint for user management
> Generate unit tests for the authentication module
> Add error handling to all database operations
```

### 🔄 Automate Workflows

```bash
# Git automation
> Analyze git commits from the last 7 days, grouped by feature
> Create a changelog from recent commits
> Find all TODO comments and create GitHub issues

# File operations
> Convert all images in this directory to PNG format
> Rename all test files to follow the *.test.ts pattern
> Find and remove all console.log statements
```

### 🐛 Debugging & Analysis

```bash
# Performance analysis
> Identify performance bottlenecks in this React component
> Find all N+1 query problems in the codebase

# Security audit
> Check for potential SQL injection vulnerabilities
> Find all hardcoded credentials or API keys
```

## Popular Tasks

### 📚 Understand New Codebases

```text
> What are the core business logic components?
> What security mechanisms are in place?
> How does the data flow through the system?
> What are the main design patterns used?
> Generate a dependency graph for this module
```

### 🔨 Code Refactoring & Optimization

```text
> What parts of this module can be optimized?
> Help me refactor this class to follow SOLID principles
> Add proper error handling and logging
> Convert callbacks to async/await pattern
> Implement caching for expensive operations
```

### 📝 Documentation & Testing

```text
> Generate comprehensive JSDoc comments for all public APIs
> Write unit tests with edge cases for this component
> Create API documentation in OpenAPI format
> Add inline comments explaining complex algorithms
> Generate a README for this module
```

### 🚀 Development Acceleration

```text
> Set up a new Express server with authentication
> Create a React component with TypeScript and tests
> Implement a rate limiter middleware
> Add database migrations for new schema
> Configure CI/CD pipeline for this project
```

## Commands & Shortcuts

### Session Commands

- `/help` - Display available commands
- `/clear` - Clear conversation history
- `/compress` - Compress history to save tokens
- `/stats` - Show current session information
- `/exit` or `/quit` - Exit Qwen Code

### Keyboard Shortcuts

- `Ctrl+C` - Cancel current operation
- `Ctrl+D` - Exit (on empty line)
- `Up/Down` - Navigate command history

## 💬 Session Management

Control your token usage with configurable session limits to optimize costs and performance.

### Configure Session Token Limit

Create or edit `.qwen/settings.json` in your home directory:

```json
{
  "sessionTokenLimit": 32000
}
```

### Session Commands

- **`/compress`** - Compress conversation history to continue within token limits
- **`/clear`** - Clear all conversation history and start fresh
- **`/stats`** - Check current token usage and limits

> [!note] 
>
> 📝 **Note**: Session token limit applies to a single conversation, not cumulative API calls.

## Vision Model Configuration

Qwen Code includes intelligent vision model auto-switching that detects images in your input and can automatically switch to vision-capable models for multimodal analysis. **This feature is enabled by default** - when you include images in your queries, you'll see a dialog asking how you'd like to handle the vision model switch.

### Skip the Switch Dialog (Optional)

If you don't want to see the interactive dialog each time, configure the default behavior in your `.qwen/settings.json`:

```json
{
  "experimental": {
    "vlmSwitchMode": "once"
  }
}
```

**Available modes:**

- **`"once"`** - Switch to vision model for this query only, then revert
- **`"session"`** - Switch to vision model for the entire session
- **`"persist"`** - Continue with current model (no switching)
- **Not set** - Show interactive dialog each time (default)

#### Command Line Override

You can also set the behavior via command line:

```bash
# Switch once per query
qwen --vlm-switch-mode once

# Switch for entire session
qwen --vlm-switch-mode session

# Never switch automatically
qwen --vlm-switch-mode persist
```

### Disable Vision Models (Optional)

To completely disable vision model support, add to your `.qwen/settings.json`:

```json
{
  "experimental": {
    "visionModelPreview": false
  }
}
```

> 👉 Know more about [Commands](https://qwenlm.github.io/qwen-code-docs/en/users/features/commands/)
>
> 💡 **Tip**: In YOLO mode (`--yolo`), vision switching happens automatically without prompts when images are detected. Know more about [Approval Mode](https://qwenlm.github.io/qwen-code-docs/en/users/features/approval-mode/)

## Benchmark Results

### Terminal-Bench Performance

| Agent     | Model              | Accuracy |
| --------- | ------------------ | -------- |
| Qwen Code | Qwen3-Coder-480A35 | 37.5%    |
| Qwen Code | Qwen3-Coder-30BA3B | 31.3%    |

## Development & Contributing

See [CONTRIBUTING.md](https://qwenlm.github.io/qwen-code-docs/en/developers/contributing/) to learn how to contribute to the project.

For detailed authentication setup, see the [authentication guide](https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/).

## Troubleshooting

If you encounter issues, check the [troubleshooting guide](https://qwenlm.github.io/qwen-code-docs/en/users/support/troubleshooting/).

## Acknowledgments

This project is based on [Google Gemini CLI](https://github.com/google-gemini/gemini-cli). We acknowledge and appreciate the excellent work of the Gemini CLI team. Our main contribution focuses on parser-level adaptations to better support Qwen-Coder models.

## License

[LICENSE](./LICENSE)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=QwenLM/qwen-code&type=Date)](https://www.star-history.com/#QwenLM/qwen-code&Date)

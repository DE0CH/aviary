## TLDR

<!-- Add a brief description of what this pull request changes and why and any important things for reviewers to look at -->

Added Chat interface to VSCode IDE Companion with support for interactive conversations with Qwen CLI, session management, and streaming responses.

<img width="2044" height="1570" alt="image" src="https://github.com/user-attachments/assets/12598d43-3f85-44be-a08e-79af12e8b73d" />

<img width="2044" height="1570" alt="image" src="https://github.com/user-attachments/assets/b743e806-a2f1-4773-9a10-2ab8959fd176" />

**Key Changes**:

- Added WebView-based Chat UI with communication to Qwen CLI
- Support for viewing, switching, and managing session lists
- Real-time streaming message display

## Dive Deeper

<!-- more thoughts and in-depth discussion here -->

**New Modules**:

- `packages/vscode-ide-companion/src/acp/AcpConnection.ts` - ACP JSON-RPC protocol implementation
- `packages/vscode-ide-companion/src/agents/QwenAgentManager.ts` - Qwen Agent lifecycle management
- `packages/vscode-ide-companion/src/services/QwenSessionReader.ts` - Read local Qwen session files (`~/.qwen/tmp/`)
- `packages/vscode-ide-companion/src/storage/ConversationStore.ts` - Conversation history persistence (VSCode GlobalState)
- `packages/vscode-ide-companion/src/WebViewProvider.ts` - WebView lifecycle management
- `packages/vscode-ide-companion/src/webview/` - React chat UI components

**Build Configuration**:

- Updated `esbuild.js` to support dual-entry bundling (extension + webview)
- Configured CSS injection plugin for stylesheet handling
- Using React 18's new JSX transform (`jsx: "react-jsx"`)

## Reviewer Test Plan

<!-- when a person reviews your code they should ideally be pulling and running that code. How would they validate your change works and if relevant what are some good classes of example prompts and ways they can exercise your changes -->

### Prerequisites

1. Ensure Qwen CLI is installed: `npm install -g @qwen/qwen-code`
2. Configure Qwen authentication (OpenAI API Key or Qwen OAuth)

### Test Steps

#### 1. Basic Functionality Test

##### Build Extension

```bash
cd packages/vscode-ide-companion
npm run build
```

Then press F5 in VSCode to launch the extension in debug mode.

#### 2. Session Management Test

- [ ] Click "📋 Sessions" button in the chat interface
- [ ] Verify existing session list is displayed
- [ ] Click "➕ New Session" to create a new session
- [ ] Switch to a historical session and verify messages load correctly
- [ ] Send messages in both new and historical sessions

#### 3. Tool Permission Test

- [ ] Send a request requiring file operations: "Create a new file hello.txt"
- [ ] Verify permission request popup appears with proper details
- [ ] Test allow/reject functionality
- [ ] Verify file operations complete as expected after permission grant

#### 4. Streaming Response Test

- [ ] Send any message to the chat
- [ ] Verify responses stream in real-time (not appearing all at once)
- [ ] Verify the streaming animation works smoothly

## Testing Matrix

<!-- Before submitting please validate your changes on as many of these options as possible -->

|          | 🍏  | 🪟  | 🐧  |
| -------- | --- | --- | --- |
| npm run  | ✅  | ❓  | ❓  |
| npx      | ❓  | ❓  | ❓  |
| Docker   | ❓  | ❓  | ❓  |
| Podman   | ❓  | -   | -   |
| Seatbelt | ❓  | -   | -   |

_Tested and verified on macOS with npm run_

## Linked issues / bugs

<!--
Link to any related issues or bugs.

**If this PR fully resolves the issue, use one of the following keywords to automatically close the issue when this PR is merged:**

- Closes #<issue_number>
- Fixes #<issue_number>
- Resolves #<issue_number>

*Example: `Resolves #123`*

**If this PR is only related to an issue or is a partial fix, simply reference the issue number without a keyword:**

*Example: `This PR makes progress on #456` or `Related to #789`*
-->

This PR adds the core chat interface functionality to the VSCode IDE Companion extension, enabling users to interact with Qwen CLI directly from VSCode with full session management capabilities.

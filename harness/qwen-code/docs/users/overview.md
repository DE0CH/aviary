# Qwen Code overview

> Learn about Qwen Code, Qwen's agentic coding tool that lives in your terminal and helps you turn ideas into code faster than ever before.

## Get started in 30 seconds

Prerequisites:

- A [Qwen Code](https://chat.qwen.ai/auth?mode=register) account
- Requires [Node.js 20+](https://nodejs.org/download), you can use `node -v` check the version. If it's not installed, use the following command to install it.

```bash
curl -qL https://www.npmjs.com/install.sh | sh
```

### Install Qwen Code:

**NPM**(recommended)

```bash
npm install -g @qwen-code/qwen-code@latest
```

**Homebrew**

```bash
brew install qwen-code
```

**GitHub**

```bash
# clone GitHub project
git clone https://github.com/QwenLM/qwen-code.git

# go to qwen-code folder
cd qwen-code

# install npm
npm install

# install qwen-code
npm install -g @qwen-code/qwen-code@latest
```

### Start using Qwen Code:

```bash
cd your-project
qwen
```

You'll be prompted to log in on first use. That's it! [Continue with Quickstart (5 mins) →](/quickstart)

> [!tip]
>
>  See [advanced setup](/setup) for installation options or [troubleshooting](/troubleshooting) if you hit issues.

> [!note]
>
> **New VS Code Extension (Beta)**: Prefer a graphical interface? Our new [VS Code extension](/vs-code) provides an easy-to-use native IDE experience without requiring terminal familiarity. Simply install from the marketplace and start coding with Qwen Code directly in your sidebar.

## What Qwen Code does for you

* **Build features from descriptions**: Tell Qwen Code what you want to build in plain language. It will make a plan, write the code, and ensure it works.
* **Debug and fix issues**: Describe a bug or paste an error message. Qwen Code will analyze your codebase, identify the problem, and implement a fix.
* **Navigate any codebase**: Ask anything about your team's codebase, and get a thoughtful answer back. Qwen Code maintains awareness of your entire project structure, can find up-to-date information from the web, and with [MCP](/mcp) can pull from external datasources like Google Drive, Figma, and Slack.
* **Automate tedious tasks**: Fix fiddly lint issues, resolve merge conflicts, and write release notes. Do all this in a single command from your developer machines, or automatically in CI.

## Why developers love Qwen Code

* **Works in your terminal**: Not another chat window. Not another IDE. Qwen Code meets you where you already work, with the tools you already love.
* **Takes action**: Qwen Code can directly edit files, run commands, and create commits. Need more? [MCP](/mcp) lets Qwen Code read your design docs in Google Drive, update your tickets in Jira, or use *your* custom developer tooling.
* **Unix philosophy**: Qwen Code is composable and scriptable. `tail -f app.log | qwen -p "Slack me if you see any anomalies appear in this log stream"` *works*. Your CI can run `qwen -p "If there are new text strings, translate them into French and raise a PR for @lang-fr-team to review"`.
* **Enterprise-ready**: Use the Qwen Code API, or host on AWS or GCP. Enterprise-grade [security](/security) and [Terms of Service](https://qwenlm.github.io/qwen-code-docs/support/tos-privacy/) is built-in.

## Next steps

<CardGroup>
	<Card title="Quickstart" icon="rocket" href="/quickstart">
    See Qwen Code in action with practical examples
  	</Card>
	
	<Card title="Common workflows" icon="graduation-cap" href="/common-workflows">
    Step-by-step guides for common workflows
  	</Card>
  	
	<Card title="Troubleshooting" icon="wrench" href="/troubleshooting">
    Solutions for common issues with Qwen Code
  	</Card>

  <Card title="IDE setup" icon="laptop" href="/vs-code">
    Add Qwen Code to your IDE
  </Card>
  
</CardGroup>

## Additional resources

<CardGroup>
  <Card title="Build with the Agent SDK" icon="code-branch" href="https://docs.claude.com/docs/agent-sdk/overview">
    Create custom AI agents with the Claude Agent SDK
  </Card>

  <Card title="Host on AWS or GCP" icon="cloud" href="/third-party-integrations">
    Configure Qwen Code with Amazon Bedrock or Google Vertex AI
  </Card>

  <Card title="Settings" icon="gear" href="/settings">
    Customize Qwen Code for your workflow
  </Card>

  <Card title="Commands" icon="terminal" href="/cli-reference">
    Learn about CLI commands and controls
  </Card>

  <Card title="Reference implementation" icon="code" href="https://github.com/anthropics/claude-code/tree/main/.devcontainer">
    Clone our development container reference implementation
  </Card>

  <Card title="Security" icon="shield" href="/security">
    Discover Qwen Code's safeguards and best practices for safe usage
  </Card>

  <Card title="Privacy and data usage" icon="lock" href="/data-usage">
    Understand how Qwen Code handles your data
  </Card>
</CardGroup>

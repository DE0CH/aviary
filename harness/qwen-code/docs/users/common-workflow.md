
# Common workflows

> Learn about common workflows with Qwen Code.

Each task in this document includes clear instructions, example commands, and best practices to help you get the most from Qwen Code.

## Understand new codebases

### Get a quick codebase overview

Suppose you've just joined a new project and need to understand its structure quickly.

1. Navigate to the project root directory

```bash  
cd /path/to/project 
```

2. Start Qwen Code

```bash  
qwen
```

3. Ask for a high-level overview

```
give me an overview of this codebase 
```

4. Dive deeper into specific components

```
explain the main architecture patterns used here 
```

```
what are the key data models?
```

```
how is authentication handled?
```

> [!tip] Tips:
> 
>   - Start with broad questions, then narrow down to specific areas
>   - Ask about coding conventions and patterns used in the project
>   - Request a glossary of project-specific terms


### Find relevant code

Suppose you need to locate code related to a specific feature or functionality.

1. Ask Qwen Code to find relevant files

```
find the files that handle user authentication 
```
  
2. Get context on how components interact

```
how do these authentication files work together? 
```

3. Understand the execution flow

```
trace the login process from front-end to database 
```

> [!tip] Tips
> 
>   - Be specific about what you're looking for
>   - Use domain language from the project

## Fix bugs efficiently

Suppose you've encountered an error message and need to find and fix its source.

1. Share the error with Qwen Code
```
I'm seeing an error when I run npm test 
```

2. Ask for fix recommendations
```
suggest a few ways to fix the @ts-ignore in user. ts 
```

3. Apply the fix
```
update user. ts to add the null check you suggested 
```

> [!tip] Tips:
> 
>   - Tell Qwen Code the command to reproduce the issue and get a stack trace
>   - Mention any steps to reproduce the error
>   - Let Qwen Code know if the error is intermittent or consistent

## Refactor code

Suppose you need to update old code to use modern patterns and practices.

1. Identify legacy code for refactoring

```
find deprecated API usage in our codebase 
```
 
2. Get refactoring recommendations

```
suggest how to refactor utils.js to use modern JavaScript features 
```
  
3. Apply the changes safely

```
refactor utils.js to use ES 2024 features while maintaining the same behavior 
```

4. Verify the refactoring

```
run tests for the refactored code 
```

> [!tip] Tips:
> 
>   - Ask Qwen Code to explain the benefits of the modern approach
>   - Request that changes maintain backward compatibility when needed
>   - Do refactoring in small, testable increments

## Use specialized subagents

Suppose you want to use specialized AI subagents to handle specific tasks more effectively.

1. View available subagents

```
/agents
```

This shows all available subagents and lets you create new ones.

2. Use subagents automatically

Qwen Code automatically delegates appropriate tasks to specialized subagents:

```
review my recent code changes for security issues
```

```
run all tests and fix any failures
```

3. Explicitly request specific subagents

```
use the code-reviewer subagent to check the auth module
```

```
have the debugger subagent investigate why users can't log in
```

4. Create custom subagents for your workflow

```
/agents
```

Then select "Create New subagent" and follow the prompts to define:

- A unique identifier that describes the subagent's purpose (for example, `code-reviewer`, `api-designer`).
- When Qwen Code should use this agent
- Which tools it can access
- A system prompt describing the agent's role and behavior

> [!tip] Tips:
> 
> - Create project-specific subagents in `.qwen-code/agents/` for team sharing
>  - Use descriptive `description` fields to enable automatic delegation
>  - Limit tool access to what each subagent actually needs
> - Check the [subagents documentation](/sub-agents) for detailed examples


## Use Plan Mode for safe code analysis

Plan Mode instructs Claude to create a plan by analyzing the codebase with read-only operations, perfect for exploring codebases, planning complex changes, or reviewing code safely.

### When to use Plan Mode

- **Multi-step implementation**: When your feature requires making edits to many files
- **Code exploration**: When you want to research the codebase thoroughly before changing anything
- **Interactive development**: When you want to iterate on the direction with Qwen Code

### How to use Plan Mode

**Turn on Plan Mode during a session**

You can switch into Plan Mode during a session using **Shift+Tab** to cycle through permission modes.

If you are in Normal Mode, **Shift+Tab** first switches into Auto-Accept Mode, indicated by `⏵⏵ accept edits on` at the bottom of the terminal. A subsequent **Shift+Tab** will switch into Plan Mode, indicated by `⏸ plan mode`.

**Start a new session in Plan Mode**

To start a new session in Plan Mode, use the `/approval-mode` then select `plan`

```bash  
/approval-mode
```

**Run "headless" queries in Plan Mode**

You can also run a query in Plan Mode directly with `-p` (that is, in ["headless mode"](/en/headless)):

```bash  
/approval-mode plan -p "Analyze the authentication system and suggest improvements"
```

### Example: Planning a complex refactor

```bash  
/approval-mode plan
```

```
I need to refactor our authentication system to use OAuth2. Create a detailed migration plan.
```

Qwen Code analyzes the current implementation and create a comprehensive plan. Refine with follow-ups:

```
What about backward compatibility?
How should we handle database migration?
```

### Configure Plan Mode as default

```json  
// .qwen-code/settings.json
{
  "permissions": {
    "defaultMode": "plan"
  }
}
```

See [settings documentation](/en/settings#available-settings) for more configuration options.

## Work with tests

Suppose you need to add tests for uncovered code.

1. Identify untested code

```
find functions in NotificationsService. swift that are not covered by tests 
```

2. Generate test scaffolding
```
add tests for the notification service 
```

3. Add meaningful test cases
```
add test cases for edge conditions in the notification service 
```

4. Run and verify tests
```
run the new tests and fix any failures 
```

Qwen Code can generate tests that follow your project's existing patterns and conventions. When asking for tests, be specific about what behavior you want to verify. Qwen Code examines your existing test files to match the style, frameworks, and assertion patterns already in use.

For comprehensive coverage, ask Qwen Code to identify edge cases you might have missed. Qwen Code can analyze your code paths and suggest tests for error conditions, boundary values, and unexpected inputs that are easy to overlook.

## Create pull requests

Suppose you need to create a well-documented pull request for your changes.

1. Summarize your changes
```
summarize the changes I've made to the authentication module 
```

  2. Generate a pull request with Qwen Code
```
create a pr 
```

  3. Review and refine
```
enhance the PR description with more context about the security improvements 
```

  4. Add testing details
```
add information about how these changes were tested 
```

> [!tip] Tips:
> 
>   - Ask Qwen Code directly to make a PR for you
>   - Review Qwen Code's generated PR before submitting
>   - Ask Qwen Code to highlight potential risks or considerations

## Handle documentation

Suppose you need to add or update documentation for your code.

1. Identify undocumented code
```
find functions without proper JSDoc comments in the auth module 
```

  2. Generate documentation
```
add JSDoc comments to the undocumented functions in auth.js 
```

  3. Review and enhance
```
improve the generated documentation with more context and examples 
```

  4. Verify documentation
```
> check if the documentation follows our project standards 
```

> [!tip] Tips:
> 
>  - Specify the documentation style you want (JSDoc, docstrings, etc.)
>  - Ask for examples in the documentation
>  - Request documentation for public APIs, interfaces, and complex logic

## Work with images

Suppose you need to work with images in your codebase, and you want Qwen Code's help analyzing image content.

1. Add an image to the conversation

You can use any of these methods:

1) Drag and drop an image into the Qwen Code window
2) Copy an image and paste it into the CLI with ctrl+v (Do not use cmd+v)
3) Provide an image path to Claude. E.g., "Analyze this image: /path/to/your/image. png"

  3. Ask Claude to analyze the image
```
What does this image show?
```

```
Describe the UI elements in this screenshot
```

```
Are there any problematic elements in this diagram?
```

  3. Use images for context
```
Here's a screenshot of the error. What's causing it?
```

```
This is our current database schema. How should we modify it for the new feature?
```

4. Get code suggestions from visual content
```
Generate CSS to match this design mockup
```

```
What HTML structure would recreate this component?
```

> [!tip] Tips:
> 
>  - Use images when text descriptions would be unclear or cumbersome
> - Include screenshots of errors, UI designs, or diagrams for better context
>  - You can work with multiple images in a conversation
>  - Image analysis works with diagrams, screenshots, mockups, and more


## Reference files and directories

Use `@` to quickly include files or directories without waiting for Qwen Code to read them.

1. Reference a single file
```
Explain the logic in @src/utils/auth.js
```

This includes the full content of the file in the conversation.

  2. Reference a directory
```
What's the structure of @src/components?
```

This provides a directory listing with file information.

  3. Reference MCP resources
```
Show me the data from @github: repos/owner/repo/issues
```

This fetches data from connected MCP servers using the format @server: resource. See [MCP resources](/en/mcp#use-mcp-resources) for details.

> [!tip] Tips:
> 
> - File paths can be relative or absolute
> - @ file references add `QWEN.md` in the file's directory and parent directories to context
> - Directory references show file listings, not contents
> - You can reference multiple files in a single message (for example, "`@file 1.js` and `@file 2.js`")


## Use extended thinking

Suppose you're working on complex architectural decisions, challenging bugs, or planning multi-step implementations that require deep reasoning.

> [!note]
> 
> [Extended thinking](https://docs.claude.com/en/docs/build-with-claude/extended-thinking) is disabled by default in Qwen Code. You can enable it on-demand by using `Tab` to toggle Thinking on, or by using prompts like "think" or "think hard". You can also enable it permanently by setting the [`MAX_THINKING_TOKENS` environment variable](/en/settings#environment-variables) in your settings.

1. Provide context and ask Qwen Code to think
```
I need to implement a new authentication system using OAuth 2 for our API. Think deeply about the best approach for implementing this in our codebase.
```

Qwen Code gathers relevant information from your codebase and uses extended thinking, which is visible in the interface.

  2. Refine the thinking with follow-up prompts
```
think about potential security vulnerabilities in this approach 
```

```
think hard about edge cases we should handle 
```

> [!tip]
> Tips to get the most value out of extended thinking:
> 
> [Extended thinking](https://docs.claude.com/en/docs/build-with-claude/extended-thinking) is most valuable for complex tasks such as:
> 
>  - Planning complex architectural changes
>  - Debugging intricate issues
>  - Creating implementation plans for new features
>  - Understanding complex codebases
>  - Evaluating tradeoffs between different approaches
> 
> Use `Tab` to toggle Thinking on and off during a session.
> 
> The way you prompt for thinking results in varying levels of thinking depth:
> 
>  - "think" triggers basic extended thinking
>  - intensifying phrases such as "keep hard", "think more", "think a lot", or "think longer" triggers deeper thinking
> 
> For more extended thinking prompting tips, see [Extended thinking tips](https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/extended-thinking-tips).

> [!note]
> Qwen Code displays its thinking process as italic gray text above the response.

## Resume previous conversations

Suppose you've been working on a task with Qwen Code and need to continue where you left off in a later session.

Qwen Code provides two options for resuming previous conversations:

 - `--continue` to automatically continue the most recent conversation
 - `--resume` to display a conversation picker

1. Continue the most recent conversation

```bash  
qwen-code --continue
```

This immediately resumes your most recent conversation without any prompts.

2. Continue in non-interactive mode

```bash  
qwen-code --continue --print "Continue with my task"
```

Use `--print` with `--continue` to resume the most recent conversation in non-interactive mode, perfect for scripts or automation.

3. Show conversation picker

```bash  
qwen-code --resume
```

This displays an interactive conversation selector with a clean list view showing:

- Session summary (or initial prompt)
- Metadata: time elapsed, message count, and git branch

Use arrow keys to navigate and press Enter to select a conversation. Press Esc to exit.

> [!tip] Tips:
> 
> - Conversation history is stored locally on your machine
> - Use `--continue` for quick access to your most recent conversation
> - Use `--resume` when you need to select a specific past conversation
> - When resuming, you'll see the entire conversation history before continuing
> - The resumed conversation starts with the same model and configuration as the original
> 
>   How it works:
> 
> 1. **Conversation Storage**: All conversations are automatically saved locally with their full message history
> 2. **Message Deserialization**: When resuming, the entire message history is restored to maintain context
>  3. **Tool State**: Tool usage and results from the previous conversation are preserved
>  4. **Context Restoration**: The conversation resumes with all previous context intact
> 
>   Examples:
> 
>   ```bash  
>   # Continue most recent conversation
>   qwen-code --continue
> 
>   # Continue most recent conversation with a specific prompt
>   qwen-code --continue --print "Show me our progress"
> 
>   # Show conversation picker
>   qwen-code --resume
> 
>   # Continue most recent conversation in non-interactive mode
>   qwen-code --continue --print "Run the tests again"
>   ```


## Run parallel Qwen Code sessions with Git worktrees

Suppose you need to work on multiple tasks simultaneously with complete code isolation between Qwen Code instances.

1. Understand Git worktrees

Git worktrees allow you to check out multiple branches from the same repository into separate directories. Each worktree has its own working directory with isolated files, while sharing the same Git history. Learn more in the [official Git worktree documentation]( https://git-scm.com/docs/git-worktree ).

2. Create a new worktree

```bash  
# Create a new worktree with a new branch 
git worktree add ../project-feature-a -b feature-a

# Or create a worktree with an existing branch
git worktree add ../project-bugfix bugfix-123
```

This creates a new directory with a separate working copy of your repository.

4. Run Qwen Code in each worktree
```bash  
# Navigate to your worktree 
cd ../project-feature-a

# Run Qwen Code in this isolated environment
qwen
```

  
5. Run Claude in another worktree
```bash  
cd ../project-bugfix
qwen
```

6. Manage your worktrees
```bash  
# List all worktrees
git worktree list

# Remove a worktree when done
git worktree remove ../project-feature-a
```


> [!tip] Tips:
> 
> - Each worktree has its own independent file state, making it perfect for parallel Qwen Code sessions
> - Changes made in one worktree won't affect others, preventing Claude instances from interfering with each other
> - All worktrees share the same Git history and remote connections
> - For long-running tasks, you can have Qwen Code working in one worktree while you continue development in another
> - Use descriptive directory names to easily identify which task each worktree is for
> - Remember to initialize your development environment in each new worktree according to your project's setup. Depending on your stack, this might include:
> 	- JavaScript projects: Running dependency installation (`npm install`, `yarn`)
> 	- Python projects: Setting up virtual environments or installing with package managers
> 	- Other languages: Following your project's standard setup process


## Use Claude as a unix-style utility

### Add Claude to your verification process

Suppose you want to use Qwen Code as a linter or code reviewer.

**Add Qwen Code to your build script:**

```json  
// package.json
{
    ...
    "scripts": {
        ...
        "lint:Qwen Code": "qwen-code -p 'you are a linter. please look at the changes vs. main and report any issues related to typos. report the filename and line number on one line, and a description of the issue on the second line. do not return any other text.'"
    }
}
```


> [!tip] Tips:
> 
> - Use Qwen Code for automated code review in your CI/CD pipeline
> - Customize the prompt to check for specific issues relevant to your project
> - Consider creating multiple scripts for different types of verification

### Pipe in, pipe out

Suppose you want to pipe data into Qwen Code, and get back data in a structured format.

**Pipe data through Claude:**

```bash  
cat build-error.txt | qwen-code -p 'concisely explain the root cause of this build error' > output.txt
```

> [!tip] Tips:
> 
> - Use pipes to integrate Qwen-Code into existing shell scripts
> - Combine with other Unix tools for powerful workflows
> - Consider using --output-format for structured output

### Control output format

Suppose you need Qwen Code's output in a specific format, especially when integrating Qwen Code into scripts or other tools.

1. Use text format (default)

```bash  
cat data. txt | qwen-code -p 'summarize this data' --output-format text > summary. txt
```

This outputs just Qwen Code's plain text response (default behavior).

2. Use JSON format

```bash  
cat code. py | qwen-code -p 'analyze this code for bugs' --output-format json > analysis.json
```

This outputs a JSON array of messages with metadata including cost and duration.

3. Use streaming JSON format

```bash  
cat log. txt | qwen-code -p 'parse this log file for errors' --output-format stream-json
```

This outputs a series of JSON objects in real-time as Qwen Code processes the request. Each message is a valid JSON object, but the entire output is not valid JSON if concatenated.
 
> [!tip] Tips:
> 
> - Use `--output-format text` for simple integrations where you just need Claude's response
> - Use `--output-format json` when you need the full conversation log
> - Use `--output-format stream-json` for real-time output of each conversation turn

## Create custom slash commands

Qwen Code supports custom slash commands that you can create to quickly execute specific prompts or tasks.

For more details, see the [Slash commands](/en/slash-commands) reference page.

### Create project-specific commands

Suppose you want to create reusable slash commands for your project that all team members can use.

1. Create a commands directory in your project

```bash  
mkdir -p .qwen-code/commands
```

2. Create a Markdown file for each command
```bash  
echo "Analyze the performance of this code and suggest three specific optimizations: " > .qwen-code/commands/optimize.md 
```

3. Use your custom command in Qwen Code
```
> /optimize 
```

> [!tip] Tips:
> 
> - Command names are derived from the filename (for example, `optimize.md` becomes `/optimize`)
> - You can organize commands in subdirectories (for example, `.qwen-code/commands/frontend/component.md` creates `/component` with " (project:frontend)" shown in the description)
> - Project commands are available to everyone who clones the repository
> - The Markdown file content becomes the prompt sent to Claude when the command is invoked

### Add command arguments with \$ARGUMENTS

Suppose you want to create flexible slash commands that can accept additional input from users.

1. Create a command file with the $ARGUMENTS placeholder
```bash  
echo 'Find and fix issue #$ARGUMENTS. Follow these steps: 
1.Understand the issue described in the ticket 
2. Locate the relevant code in
our codebase 
3. Implement a solution that addresses the root cause 
4. Add
appropriate tests 
5. Prepare a concise PR description' >
.qwen-code/commands/fix-issue.md 
```

  2. Use the command with an issue number
  In your Claude session, use the command with arguments.

```
> /fix-issue 123 
```

This replaces \$ARGUMENTS with "123" in the prompt.

> [!tip] Tips:
> 
>  - The \$ARGUMENTS placeholder is replaced with any text that follows the command
>  - You can position \$ARGUMENTS anywhere in your command template
>  - Other useful applications: generating test cases for specific functions, creating documentation for components, reviewing code in particular files, or translating content to specified languages

### Create personal slash commands

Suppose you want to create personal slash commands that work across all your projects.

1. Create a commands directory in your home folder
```bash  
mkdir -p ~/.qwen-code/commands 
```

  2. Create a Markdown file for each command
```bash  
echo "Review this code for security vulnerabilities, focusing on: " >
~/.qwen-code/commands/security-review.md 
```

3. Use your personal custom command
```
> /security-review 
```

> [!tip] Tips:
> 
> - Personal commands show " (user)" in their description when listed with `/help`
> - Personal commands are only available to you and not shared with your team
> - Personal commands work across all your projects
> - You can use these for consistent workflows across different codebases

## Ask Qwen Code about its capabilities

Qwen Code has built-in access to its documentation and can answer questions about its own features and limitations.

### Example questions

```
can Qwen Code create pull requests?
```

```
how does Qwen Code handle permissions?
```

```
what slash commands are available?
```

```
how do I use MCP with Qwen Code?
```

```
how do I configure Qwen Code for Amazon Bedrock?
```

```
what are the limitations of Qwen Code?
```

> [!note]
> 
> Qwen Code provides documentation-based answers to these questions. For executable examples and hands-on demonstrations, refer to the specific workflow sections above.

> [!tip] Tips:
> 
> - Qwen Code always has access to the latest Qwen Code documentation, regardless of the version you're using
> - Ask specific questions to get detailed answers
> - Qwen Code can explain complex features like MCP integration, enterprise configurations, and advanced workflows

## Next steps

<Card title="Qwen Code reference implementation" icon="code" href="https://github.com/anthropics/claude-code/tree/main/.devcontainer
  Clone our development container reference implementation.
</Card>

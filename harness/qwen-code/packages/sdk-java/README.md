# Qwen Code Java SDK

A minimum experimental Java SDK for programmatic access to Qwen Code functionality. This SDK provides a Java interface to interact with the Qwen Code CLI, allowing developers to integrate Qwen Code capabilities into their Java applications.

Feel free to submit a feature request/issue/PR.

## Installation

Add the following dependency to your Maven `pom.xml`:

```xml
<dependency>
    <groupId>com.alibaba</groupId>
    <artifactId>qwencode-sdk-java</artifactId>
    <version>0.0.1</version>
</dependency>
```

Or if using Gradle, add to your `build.gradle`:

```gradle
implementation 'com.alibaba:qwencode-sdk-java:0.0.1'
```

## Requirements

- Java >= 1.8
- Maven >= 3.6.0 (for building from source)
- Qwen Code CLI: The SDK communicates with the Qwen Code CLI executable. By default, the SDK looks for a `qwen` command in the system PATH.

## Quick Start

The simplest way to use the SDK is through the `QwenCodeCli.simpleQuery()` method:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import java.util.List;

public class Example {
    public static void main(String[] args) {
        List<String> result = QwenCodeCli.simpleQuery("hello world");
        result.forEach(System.out::println);
    }
}
```

For more advanced usage with streaming responses:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import java.util.function.Consumer;

public class StreamingExample {
    public static void main(String[] args) {
        QwenCodeCli.simpleQuery("hello world", (String message) -> {
            System.out.println("Received: " + message);
        });
    }
}
```

For session-based usage with custom event handling:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.utils.Timeout;
import java.util.concurrent.TimeUnit;

public class SessionExample {
    public static void main(String[] args) {
        try (Session session = QwenCodeCli.newSession()) {
            SessionEventSimpleConsumers eventConsumers = new SessionEventSimpleConsumers() {
                @Override
                public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
                    String message = assistantMessage.getMessage().getContent().stream()
                        .findFirst()
                        .map(content -> content.getText())
                        .orElse("");
                    System.out.println("Assistant: " + message);
                }
            }.setDefaultEventTimeout(new Timeout(60L, TimeUnit.SECONDS));

            session.sendPrompt("hello world", eventConsumers);
        } catch (Exception e) {
            e.printStackTrace();
        }
    }
}
```

## Architecture

The Qwen Code Java SDK follows a layered architecture that abstracts the communication with the Qwen Code CLI:

### Layered Architecture

- **API Layer**: Provides the main entry points through `QwenCodeCli` class with simple static methods for basic usage
- **Session Layer**: Manages communication sessions with the Qwen Code CLI through the `Session` class
- **Transport Layer**: Handles the communication mechanism between the SDK and CLI process (currently using process transport via `ProcessTransport`)
- **Protocol Layer**: Defines data structures for communication based on the CLI protocol
- **Utils**: Common utilities for concurrent execution, timeout handling, and error management

### Core Classes and Their Relationships

- `QwenCodeCli`: The main entry point that provides static methods (`simpleQuery`) which internally create and manage `Session` instances
- `Session`: Manages the lifecycle of a communication session with the CLI, including initialization, prompt sending, and cleanup
- `Transport`: Abstracts the communication mechanism (currently implemented by `ProcessTransport`)
- `ProcessTransport`: Implementation that communicates with the CLI via process execution, using `TransportOptions` for configuration
- `TransportOptions`: Configuration class that defines how the transport layer should interact with the CLI (path to executable, working directory, model, permission mode, etc.)
- `SessionEventSimpleConsumers`: Event handler interface for processing responses from the CLI, allowing custom handling of assistant messages and other events
- `PermissionMode`: Enum that defines different permission modes for controlling tool execution (default, plan, auto-edit, yolo)

The architecture allows for both simple usage through static methods in `QwenCodeCli` and more advanced usage through direct `Session` management with custom event handlers and transport options.

## Usage

### Session Event Consumers

The SDK allows you to customize how events from the CLI are handled using event consumers. The `SessionEventConsumers` interface provides callbacks for different types of messages during a session:

- `onSystemMessage`: Handles system messages from the CLI (receives Session and SDKSystemMessage)
- `onResultMessage`: Handles result messages from the CLI (receives Session and SDKResultMessage)
- `onAssistantMessage`: Handles assistant messages (AI responses) (receives Session and SDKAssistantMessage)
- `onPartialAssistantMessage`: Handles partial assistant messages during streaming (receives Session and SDKPartialAssistantMessage)
- `onUserMessage`: Handles user messages (receives Session and SDKUserMessage)
- `onOtherMessage`: Handles other types of messages (receives Session and String message)
- `onControlResponse`: Handles control responses (receives Session and CLIControlResponse)
- `onControlRequest`: Handles control requests (receives Session and CLIControlRequest, returns CLIControlResponse)
- `onPermissionRequest`: Handles permission requests (receives Session and CLIControlRequest<CLIControlPermissionRequest>, returns Behavior)
- `onAssistantMessageIncludePartial`: Handles assistant messages including partial content (specific to SessionEventSimpleConsumers, called by both onAssistantMessage and onPartialAssistantMessage) (receives Session, List<AssistantContent>, and AssistantMessageOutputType)

Event processing is subject to the timeout settings configured in `TransportOptions` and `SessionEventConsumers`. For detailed timeout configuration options, see the "Timeout" section above.

Example of custom event handling:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKPartialAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.utils.Timeout;

import java.util.List;
import java.util.concurrent.TimeUnit;

public class CustomEventHandlingExample {
    public static void main(String[] args) {
        Session session = QwenCodeCli.newSession();
        SessionEventSimpleConsumers eventConsumers = new SessionEventSimpleConsumers() {
            @Override
            public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
                String message = assistantMessage.getMessage().getContent().stream()
                        .findFirst()
                        .map(content -> content.getText())
                        .orElse("");
                System.out.println("Assistant: " + message);
            }

            @Override
            public void onPartialAssistantMessage(Session session, SDKPartialAssistantMessage partialAssistantMessage) {
                System.out.println("Partial assistant message: " + partialAssistantMessage);
            }

            public void onAssistantMessageIncludePartial(Session session, List<AssistantContent> assistantContents,
                    AssistantMessageOutputType assistantMessageOutputType) {
                System.out.println("Assistant content (type: " + assistantMessageOutputType + "): " + assistantContents);
            }

            @Override
            public void onSystemMessage(Session session, SDKSystemMessage systemMessage) {
                System.out.println("System: " + systemMessage.getMessage());
            }

            @Override
            public void onResultMessage(Session session, SDKResultMessage resultMessage) {
                System.out.println("Result: " + resultMessage.getMessage());
            }

            @Override
            public void onUserMessage(Session session, SDKUserMessage userMessage) {
                System.out.println("User: " + userMessage.getMessage());
            }

            @Override
            public void onOtherMessage(Session session, String message) {
                System.out.println("Other: " + message);
            }

            @Override
            public void onControlResponse(Session session, CLIControlResponse<?> cliControlResponse) {
                System.out.println("Control response: " + cliControlResponse);
            }

            @Override
            public CLIControlResponse<?> onControlRequest(Session session, CLIControlRequest<?> cliControlRequest) {
                System.out.println("Control request: " + cliControlRequest);
                return new CLIControlResponse<>(); // Return appropriate response
            }

            @Override
            public Behavior onPermissionRequest(Session session, CLIControlRequest<CLIControlPermissionRequest> permissionRequest) {
                System.out.println("Permission request: " + permissionRequest.getRequest().getInput());
                return new com.alibaba.qwen.code.cli.protocol.data.behavior.Allow()
                        .setUpdatedInput(permissionRequest.getRequest().getInput()); // Allow by default
            }

            @Override
            public Timeout onAssistantMessageTimeout(Session session) {
                return new Timeout(90L, TimeUnit.SECONDS);  // Timeout for processing assistant messages
            }

            @Override
            public Timeout onSystemMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing system messages
            }

            @Override
            public Timeout onResultMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing result messages
            }

            @Override
            public Timeout onPartialAssistantMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing partial assistant messages
            }

            @Override
            public Timeout onUserMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing user messages
            }

            @Override
            public Timeout onOtherMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing other messages
            }

            @Override
            public Timeout onControlResponseTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing control responses
            }

            @Override
            public Timeout onControlRequestTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing control requests
            }

            @Override
            public Timeout onPermissionRequestTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing permission requests
            }
        }.setDefaultEventTimeout(new Timeout(60L, TimeUnit.SECONDS));  // Default timeout for all events

        session.sendPrompt("Example prompt", eventConsumers);
    }
}
```

### Permission Modes

The SDK supports different permission modes for controlling tool execution:

- **`default`**: Write tools are denied unless approved via `canUseTool` callback or in `allowedTools`. Read-only tools execute without confirmation.
- **`plan`**: Blocks all write tools, instructing AI to present a plan first.
- **`auto-edit`**: Auto-approve edit tools (edit, write_file) while other tools require confirmation.
- **`yolo`**: All tools execute automatically without confirmation.

To set a permission mode:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;

public class PermissionModeExample {
    public static void main(String[] args) {
        Session session = QwenCodeCli.newSession(new TransportOptions().setPermissionMode(PermissionMode.YOLO));
        session.setPermissionMode(PermissionMode.PLAN);
    }
}
```

### Session Control

The SDK provides fine-grained control over session lifecycle and behavior:

- **Session creation**: Use `QwenCodeCli.newSession()` to create a new session with custom options
- **Session management**: The `Session` class provides methods to send prompts, handle responses, and manage session state
- **Session cleanup**: Always close sessions using `session.close()` to properly terminate the CLI process
- **Session resumption**: Use `setResumeSessionId()` in `TransportOptions` to resume a previous session
- **Session interruption**: Use `session.interrupt()` to interrupt a currently running prompt
- **Dynamic model switching**: Use `session.setModel()` to change the model during a session
- **Dynamic permission mode switching**: Use `session.setPermissionMode()` to change the permission mode during a session

Example of session control:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import java.util.List;

public class SessionControlExample {
    public static void main(String[] args) {
        TransportOptions options = new TransportOptions()
            .setModel("qwen-max")
            .setPermissionMode(PermissionMode.AUTO_EDIT);

        try (Session session = QwenCodeCli.newSession(options)) {
            // Use the session with default event consumers
            List<String> result = session.sendPrompt("Explain how to use the SDK", new SessionEventSimpleConsumers());
            result.forEach(System.out::println);
        } // Session automatically closes when exiting try-with-resources
    }
}
```

#### Interrupt Function

The `interrupt()` function allows you to interrupt a currently running prompt. This is useful when you need to stop a long-running operation without closing the entire session:

- **Method signature**: `public Optional<Boolean> interrupt() throws SessionControlException`
- **Purpose**: Interrupts the current prompt processing without closing the session
- **Return value**: An `Optional<Boolean>` that indicates whether the interrupt was successful (true if successful, empty if the interrupt was sent asynchronously)

Example of interrupting a running prompt:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.session.exception.SessionControlException;
import java.util.Optional;

public class InterruptExample {
    public static void main(String[] args) {
        try (Session session = QwenCodeCli.newSession()) {
            session.sendPrompt("Analyze this large codebase...", new SessionEventSimpleConsumers() {
                @Override
                public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
                    System.out.println("Received: " + assistantMessage.getMessage().getContent().stream()
                        .findFirst()
                        .map(content -> content.getText())
                        .orElse(""));

                    // Interrupt the session after receiving the first message
                    try {
                        Optional<Boolean> interruptResult = session.interrupt();
                        System.out.println(interruptResult.map(s -> s ? "Interrupt successful" : "Interrupt error")
                                .orElse("Interrupt unknown"));
                    } catch (SessionControlException e) {
                        System.err.println("Interrupt error: " + e.getMessage());
                    }
                }
            });
        }
    }
}
```

#### Set Model Function

The `setModel()` function allows you to dynamically change the AI model during an active session. This is useful when you want to switch between different models (e.g., from a faster model for simple queries to a more powerful model for complex analysis) without creating a new session:

- **Method signature**: `public Optional<Boolean> setModel(String modelName) throws SessionControlException`
- **Purpose**: Changes the AI model being used for the current and subsequent prompts in the session
- **Parameters**: `modelName` - the name of the model to switch to (e.g., "qwen-max", "qwen-plus", etc.)
- **Return value**: An `Optional<Boolean>` that indicates whether the model change was successful (true if successful, empty if the request was sent asynchronously)

Example of changing the model during a session:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import java.util.Optional;

public class SetModelExample {
    public static void main(String[] args) {
        try (Session session = QwenCodeCli.newSession()) {
            // Switch to a specific model
            Optional<Boolean> modelChangeResult = session.setModel("qwen3-coder-flash");
            System.out.println(modelChangeResult.map(s -> s ? "setModel success" : "setModel error")
                    .orElse("setModel unknown"));

            // Use the model for a prompt
            session.sendPrompt("hello world", new SessionEventSimpleConsumers());

            // Switch to another model
            Optional<Boolean> modelChangeResult2 = session.setModel("qwen3-coder-plus");
            System.out.println(modelChangeResult2.map(s -> s ? "setModel success" : "setModel error")
                    .orElse("setModel unknown"));

            // Use the new model for another prompt
            session.sendPrompt("list files in the current directory", new SessionEventSimpleConsumers());
        }
    }
}
```

#### Set Permission Mode Function

The `setPermissionMode()` function allows you to dynamically change the permission mode during an active session. This is useful when you want to adjust the level of access granted to tools (e.g., switching from a restrictive mode to allow more operations) without creating a new session:

- **Method signature**: `public Optional<Boolean> setPermissionMode(PermissionMode permissionMode) throws SessionControlException`
- **Purpose**: Changes the permission mode governing tool execution for the current and subsequent prompts in the session
- **Parameters**: `permissionMode` - the permission mode to switch to (e.g., `PermissionMode.DEFAULT`, `PermissionMode.PLAN`, `PermissionMode.AUTO_EDIT`, `PermissionMode.YOLO`)
- **Return value**: An `Optional<Boolean>` that indicates whether the permission mode change was successful (true if successful, empty if the request was sent asynchronously)

Example of changing the permission mode during a session:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import java.util.Optional;

public class SetPermissionModeExample {
    public static void main(String[] args) {
        try (Session session = QwenCodeCli.newSession()) {
            // Switch to a permissive mode
            Optional<Boolean> permissionChangeResult = session.setPermissionMode(PermissionMode.YOLO);
            System.out.println(permissionChangeResult.map(s -> s ? "setPermissionMode success" : "setPermissionMode error")
                    .orElse("setPermissionMode unknown"));

            // Use the session with the new permission mode
            session.sendPrompt("in the dir src/test/temp/, create file empty file test.touch", new SessionEventSimpleConsumers());

            // Switch to another permission mode
            Optional<Boolean> permissionChangeResult2 = session.setPermissionMode(PermissionMode.PLAN);
            System.out.println(permissionChangeResult2.map(s -> s ? "setPermissionMode success" : "setPermissionMode error")
                    .orElse("setPermissionMode unknown"));

            // Use the session with the new permission mode
            session.sendPrompt("rename test.touch to test_rename.touch", new SessionEventSimpleConsumers());
        }
    }
}
```

### Timeout Configuration

The timeout configuration allows you to control how long the SDK waits for responses from the CLI before timing out. There are two levels of timeout configuration:

- **Transport-level timeouts**: Configured via `TransportOptions`
  - `turnTimeout`: Time to wait for a complete turn of conversation (default: 60 seconds)
  - `messageTimeout`: Time to wait for individual messages within a turn (default: 60 seconds)

- **Event-level timeouts**: Configured via `SessionEventConsumers` interface with callback methods for specific message types:
  - `onSystemMessageTimeout`: Timeout for processing system messages
  - `onResultMessageTimeout`: Timeout for processing result messages
  - `onAssistantMessageTimeout`: Timeout for processing assistant messages
  - `onPartialAssistantMessageTimeout`: Timeout for processing partial assistant messages
  - `onUserMessageTimeout`: Timeout for processing user messages
  - `onOtherMessageTimeout`: Timeout for processing other types of messages
  - `onControlResponseTimeout`: Timeout for processing control responses
  - `onControlRequestTimeout`: Timeout for processing control requests
  - `onPermissionRequestTimeout`: Timeout for processing permission requests

To customize timeout settings:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventConsumers;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.utils.Timeout;
import java.util.List;
import java.util.concurrent.TimeUnit;

public class TimeoutConfigurationExample {
    public static void main(String[] args) {
        // Configure transport-level timeouts
        TransportOptions options = new TransportOptions()
            .setTurnTimeout(new Timeout(120L, TimeUnit.SECONDS))    // Timeout for a complete turn of conversation
            .setMessageTimeout(new Timeout(90L, TimeUnit.SECONDS));  // Timeout for individual messages within a turn

        Session session = QwenCodeCli.newSession(options);

        // Configure event-level timeouts using SessionEventConsumers
        SessionEventConsumers eventConsumers = new SessionEventSimpleConsumers() {
            @Override
            public Timeout onSystemMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing system messages
            }

            @Override
            public Timeout onResultMessageTimeout(Session session) {
                return new Timeout(60L, TimeUnit.SECONDS);  // Timeout for processing result messages
            }

            @Override
            public Timeout onAssistantMessageTimeout(Session session) {
                return new Timeout(90L, TimeUnit.SECONDS);  // Timeout for processing assistant messages
            }

            @Override
            public Timeout onControlResponseTimeout(Session session) {
                return new Timeout(45L, TimeUnit.SECONDS);  // Timeout for processing control responses
            }

            @Override
            public Timeout onPermissionRequestTimeout(Session session) {
                return new Timeout(30L, TimeUnit.SECONDS);  // Timeout for processing permission requests
            }

            @Override
            public Timeout onOtherMessageTimeout(Session session) {
                return new Timeout(35L, TimeUnit.SECONDS);  // Timeout for processing other messages
            }
        }.setDefaultEventTimeout(new Timeout(90L, TimeUnit.SECONDS));  // Default timeout for all events
        session.sendPrompt("hello world", eventConsumers);
    }
}
```

### Thread Pool Configuration

The SDK uses a thread pool for managing concurrent operations. The default thread pool configuration is defined in the `ThreadPoolConfig` class:

- **Core Pool Size**: 10 threads
- **Maximum Pool Size**: 30 threads
- **Keep-Alive Time**: 60 seconds
- **Queue Capacity**: 300 tasks (using LinkedBlockingQueue)
- **Thread Naming**: "qwen_code_cli-pool-{number}"
- **Daemon Threads**: false
- **Rejected Execution Handler**: CallerRunsPolicy (executes the task on the calling thread when the pool is full)

The thread pool can be customized in two ways:

1. **Using a custom supplier**: Provide a custom `Supplier<ThreadPoolExecutor>` through the `ThreadPoolConfig.setExecutorSupplier()` method. If no custom supplier is provided, or if the supplier throws an exception, the SDK will fall back to the default thread pool configuration.

2. **Modifying properties after getting the default executor**: You can retrieve the default executor using `ThreadPoolConfig.getDefaultExecutor()` and then modify its properties such as core pool size, maximum pool size, and keep-alive time.

Example of custom thread pool configuration using a supplier:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.ThreadPoolConfig;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.function.Supplier;

public class ThreadPoolConfigurationExample {
    public static void main(String[] args) {
        // Set a custom thread pool supplier
        ThreadPoolConfig.setExecutorSupplier(new Supplier<ThreadPoolExecutor>() {
            @Override
            public ThreadPoolExecutor get() {
                return (ThreadPoolExecutor) Executors.newFixedThreadPool(20);
            }
        });

        // The SDK will now use the custom thread pool for all operations
        Session session = QwenCodeCli.newSession();
    }
}
```

Example of modifying properties after getting the default executor:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.ThreadPoolConfig;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

public class ModifyThreadPoolExample {
    public static void main(String[] args) {
        // Get the default executor and modify its properties
        ThreadPoolExecutor executor = ThreadPoolConfig.getDefaultExecutor();

        // Modify the core pool size
        executor.setCorePoolSize(15);

        // Modify the maximum pool size
        executor.setMaximumPoolSize(40);

        // Modify the keep-alive time
        executor.setKeepAliveTime(120, TimeUnit.SECONDS);

        // The SDK will now use the modified executor for all operations
        Session session = QwenCodeCli.newSession();
    }
}
```

Note that when modifying the default executor directly, you're changing the properties of the shared static instance that will affect all subsequent operations in the application. If you need different configurations for different parts of your application, using the supplier approach is recommended.

### Transport Options

The `TransportOptions` class allows you to configure how the SDK communicates with the Qwen Code CLI. Below are all the available options with their descriptions:

- **`pathToQwenExecutable`**: Specifies the path to the Qwen Code CLI executable. By default, the SDK looks for a `qwen` command in the system PATH.
  - Type: `String`
  - Example: `new TransportOptions().setPathToQwenExecutable("/usr/local/bin/qwen")`

- **`cwd`**: Sets the working directory for the CLI process. This affects where the CLI operates and where relative paths are resolved from.
  - Type: `String`
  - Example: `new TransportOptions().setCwd("/path/to/project")`

- **`model`**: Specifies the AI model to use for the session (e.g., "qwen-max", "qwen-plus", "qwen3-coder-flash", etc.).
  - Type: `String`
  - Example: `new TransportOptions().setModel("qwen3-coder-flash")`

- **`permissionMode`**: Sets the permission mode that controls tool execution. Available modes are:
  - `PermissionMode.DEFAULT`: Write tools are denied unless approved via `canUseTool` callback or in `allowedTools`. Read-only tools execute without confirmation.
  - `PermissionMode.PLAN`: Blocks all write tools, instructing AI to present a plan first.
  - `PermissionMode.AUTO_EDIT`: Auto-approve edit tools (edit, write_file) while other tools require confirmation.
  - `PermissionMode.YOLO`: All tools execute automatically without confirmation.
  - Type: `PermissionMode`
  - Example: `new TransportOptions().setPermissionMode(PermissionMode.YOLO)`

- **`env`**: A map of environment variables to pass to the CLI process.
  - Type: `Map<String, String>`
  - Example: `new TransportOptions().setEnv(Map.of("ENV_VAR", "value"))`

- **`maxSessionTurns`**: Limits the number of conversation turns in a session.
  - Type: `Integer`
  - Example: `new TransportOptions().setMaxSessionTurns(10)`

- **`coreTools`**: Specifies a list of core tools that should be available to the AI.
  - Type: `List<String>`
  - Example: `new TransportOptions().setCoreTools(List.of("read_file", "write_file"))`

- **`excludeTools`**: Specifies a list of tools to exclude from being available to the AI.
  - Type: `List<String>`
  - Example: `new TransportOptions().setExcludeTools(List.of("shell"))`

- **`allowedTools`**: Specifies a list of tools that are pre-approved for use without additional confirmation.
  - Type: `List<String>`
  - Example: `new TransportOptions().setAllowedTools(List.of("read_file", "list_directory"))`

- **`authType`**: Specifies the authentication type to use for the session.
  - Type: `String`
  - Example: `new TransportOptions().setAuthType("bearer")`

- **`includePartialMessages`**: When true, enables receiving partial messages during streaming responses.
  - Type: `Boolean`
  - Example: `new TransportOptions().setIncludePartialMessages(true)`

- **`skillsEnable`**: Enables or disables skills functionality for the session.
  - Type: `Boolean`
  - Example: `new TransportOptions().setSkillsEnable(true)`

- **`turnTimeout`**: Sets the timeout for a complete turn of conversation (default: 60 seconds).
  - Type: `Timeout`
  - Example: `new TransportOptions().setTurnTimeout(new Timeout(120L, TimeUnit.SECONDS))`

- **`messageTimeout`**: Sets the timeout for individual messages within a turn (default: 60 seconds).
  - Type: `Timeout`
  - Example: `new TransportOptions().setMessageTimeout(new Timeout(90L, TimeUnit.SECONDS))`

- **`resumeSessionId`**: Specifies the ID of a previous session to resume.
  - Type: `String`
  - Example: `new TransportOptions().setResumeSessionId("session-12345")`

- **`otherOptions`**: Allows passing additional command-line options directly to the CLI.
  - Type: `List<String>`
  - Example: `new TransportOptions().setOtherOptions(List.of("--verbose", "--no-cache"))`

Example of using TransportOptions:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import com.alibaba.qwen.code.cli.utils.Timeout;
import java.util.List;
import java.util.Map;
import java.util.concurrent.TimeUnit;

public class TransportOptionsExample {
    public static void main(String[] args) {
        TransportOptions options = new TransportOptions()
            .setModel("qwen3-coder-flash")
            .setPermissionMode(PermissionMode.AUTO_EDIT)
            .setCwd("/path/to/working/directory")
            .setEnv(Map.of("CUSTOM_VAR", "value"))
            .setIncludePartialMessages(true)
            .setTurnTimeout(new Timeout(120L, TimeUnit.SECONDS))
            .setMessageTimeout(new Timeout(90L, TimeUnit.SECONDS))
            .setAllowedTools(List.of("read_file", "write_file", "list_directory"));

        try (Session session = QwenCodeCli.newSession(options)) {
            // Use the session with custom options
            List<String> result = session.sendPrompt("Analyze the current project", new SessionEventSimpleConsumers());
            result.forEach(System.out::println);
        }
    }
}
```

### Error Handling

The SDK provides specific exception types for different error scenarios:

- `SessionControlException`: Thrown when there's an issue with session control (creation, initialization, etc.)
- `SessionSendPromptException`: Thrown when there's an issue sending a prompt or receiving a response
- `SessionClosedException`: Thrown when attempting to use a closed session

Example of comprehensive error handling:

```java
import com.alibaba.qwen.code.cli.QwenCodeCli;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.session.exception.SessionControlException;
import com.alibaba.qwen.code.cli.session.exception.SessionSendPromptException;
import java.util.List;

public class ErrorHandlingExample {
    public static void main(String[] args) {
        try (Session session = QwenCodeCli.newSession()) {
            try {
                List<String> result = session.sendPrompt("Process this request", new SessionEventSimpleConsumers());
                result.forEach(System.out::println);
            } catch (SessionSendPromptException e) {
                System.err.println("Error sending prompt: " + e.getMessage());
                e.printStackTrace();
            }
        } catch (SessionControlException e) {
            System.err.println("Error controlling session: " + e.getMessage());
            e.printStackTrace();
        } catch (Exception e) {
            System.err.println("Unexpected error: " + e.getMessage());
            e.printStackTrace();
        }
    }
}
```

## FAQ / Troubleshooting

### Q: Do I need to install the Qwen CLI separately?

A: No, from v0.1.1, the CLI is bundled with the SDK, so no standalone CLI installation is needed.

### Q: What Java versions are supported?

A: The SDK requires Java 1.8 or higher.

### Q: How do I handle long-running requests?

A: The SDK includes timeout utilities. You can configure timeouts using the `Timeout` class in `TransportOptions`.

### Q: Why are some tools not executing?

A: This is likely due to permission modes. Check your permission mode settings and consider using `allowedTools` to pre-approve certain tools.

### Q: How do I resume a previous session?

A: Use the `setResumeSessionId()` method in `TransportOptions` to resume a previous session.

### Q: Can I customize the environment for the CLI process?

A: Yes, use the `setEnv()` method in `TransportOptions` to pass environment variables to the CLI process.

### Q: What happens if the CLI process crashes?

A: The SDK will throw appropriate exceptions. Make sure to handle `SessionControlException` and implement retry logic if needed.

## License

Apache-2.0 - see [LICENSE](./LICENSE) for details.

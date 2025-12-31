# Qwen Code Java SDK

## Project Overview

The Qwen Code Java SDK is a minimum experimental SDK for programmatic access to Qwen Code functionality. It provides a Java interface to interact with the Qwen Code CLI, allowing developers to integrate Qwen Code capabilities into their Java applications.

The project is structured as a Maven-based Java library with the following key characteristics:

- **Group ID**: com.alibaba
- **Artifact ID**: qwencode-sdk-java
- **Version**: 0.0.1
- **Packaging**: JAR
- **Java Version**: 1.8+ (source and target)

## Architecture

The SDK follows a layered architecture:

- **CLI Layer**: Provides the main entry point through `QwenCodeCli` class
- **Session Layer**: Manages communication sessions with the Qwen Code CLI
- **Transport Layer**: Handles communication between the SDK and CLI process
- **Protocol Layer**: Defines data structures for communication
- **Utils**: Common utilities for concurrent execution and timeout handling

## Key Components

### Main Classes

- `QwenCodeCli`: Main entry point with static methods for simple queries
- `Session`: Manages communication sessions with the CLI
- `Transport`: Abstracts the communication mechanism (currently using process transport)
- `ProcessTransport`: Implementation that communicates via process execution

### Dependencies

- **Logging**: ch.qos.logback:logback-classic
- **Utilities**: org.apache.commons:commons-lang3
- **JSON Processing**: com.alibaba.fastjson2:fastjson2
- **Testing**: JUnit 5 (org.junit.jupiter:junit-jupiter)

## Building and Running

### Prerequisites

- Java 8 or higher
- Apache Maven 3.6.0 or higher

### Build Commands

```bash
# Compile the project
mvn compile

# Run tests
mvn test

# Package the JAR
mvn package

# Install to local repository
mvn install

# Run checkstyle verification
mvn checkstyle:check
```

### Testing

The project includes basic unit tests using JUnit 5. The main test class `QwenCodeCliTest` demonstrates how to use the SDK to make simple queries to the Qwen Code CLI.

### Code Quality

The project uses Checkstyle for code formatting and style enforcement. The configuration is defined in `checkstyle.xml` and includes rules for:

- Whitespace and indentation
- Naming conventions
- Import ordering
- Code structure

## Development Conventions

### Coding Standards

- Java 8 language features are supported
- Follow standard Java naming conventions
- Use UTF-8 encoding for source files
- Line endings should be LF (Unix-style)
- No trailing whitespace allowed
- Use 8-space indentation for line wrapping

### Testing Practices

- Write unit tests using JUnit 5
- Test classes should be in the `src/test/java` directory
- Follow the naming convention `*Test.java` for test classes
- Use appropriate assertions to validate functionality

### Documentation

- API documentation should follow JavaDoc conventions
- Update README files when adding new features
- Include examples in documentation

## API Reference

### QwenCodeCli Class

The main class provides two primary methods:

- `simpleQuery(String prompt)`: Synchronous method that returns a list of responses
- `simpleQuery(String prompt, Consumer<String> messageConsumer)`: Asynchronous method that streams responses to a consumer

### Permission Modes

The SDK supports different permission modes for controlling tool execution:

- **`default`**: Write tools are denied unless approved via `canUseTool` callback or in `allowedTools`. Read-only tools execute without confirmation.
- **`plan`**: Blocks all write tools, instructing AI to present a plan first.
- **`auto-edit`**: Auto-approve edit tools (edit, write_file) while other tools require confirmation.
- **`yolo`**: All tools execute automatically without confirmation.

## Usage Example

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

## Project Structure

```
src/
├── main/
│   └── java/
│       └── com/
│           └── alibaba/
│               └── qwen/
│                   └── code/
│                       └── cli/
│                           ├── QwenCodeCli.java
│                           ├── protocol/
│                           ├── session/
│                           ├── transport/
│                           └── utils/
└── test/
    └── java/
        └── com/
            └── alibaba/
                └── qwen/
                    └── code/
                        └── cli/
                            └── QwenCodeCliTest.java
```

## Configuration Files

- `pom.xml`: Maven build configuration and dependencies
- `checkstyle.xml`: Code style and formatting rules
- `.editorconfig`: Editor configuration settings

## License

Apache-2.0 - see [LICENSE](./LICENSE) for details.

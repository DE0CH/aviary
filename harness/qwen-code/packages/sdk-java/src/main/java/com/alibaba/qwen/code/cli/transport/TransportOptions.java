package com.alibaba.qwen.code.cli.transport;

import java.util.List;
import java.util.Map;
import java.util.function.Consumer;

public class TransportOptions {
    private String pathToQwenExecutable;
    private String cwd;
    private String model;
    private PermissionMode permissionMode;
    private Map<String, String> env;
    private Object abortController; // AbortController in JavaScript does not have a direct Java equivalent
    private Boolean debug;
    private Consumer<String> stderr; // Equivalent to (message: string) => void
    private String logLevel; // Can be 'debug', 'info', 'warn', or 'error'
    private Integer maxSessionTurns;
    private List<String> coreTools;
    private List<String> excludeTools;
    private List<String> allowedTools;
    private String authType;
    private Boolean includePartialMessages;
}

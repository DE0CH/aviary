package com.alibaba.qwen.code.cli.transport;

import java.util.List;
import java.util.Map;

public class TransportOptions implements Cloneable {
    private String pathToQwenExecutable;
    private String cwd;
    private String model;
    private PermissionMode permissionMode;
    private Map<String, String> env;
    private Integer maxSessionTurns;
    private List<String> coreTools;
    private List<String> excludeTools;
    private List<String> allowedTools;
    private String authType;
    private Boolean includePartialMessages;
    private Long turnTimeoutMs;
    private Long messageTimeoutMs;

    public String getPathToQwenExecutable() {
        return pathToQwenExecutable;
    }

    public void setPathToQwenExecutable(String pathToQwenExecutable) {
        this.pathToQwenExecutable = pathToQwenExecutable;
    }

    public String getCwd() {
        return cwd;
    }

    public void setCwd(String cwd) {
        this.cwd = cwd;
    }

    public String getModel() {
        return model;
    }

    public void setModel(String model) {
        this.model = model;
    }

    public PermissionMode getPermissionMode() {
        return permissionMode;
    }

    public void setPermissionMode(PermissionMode permissionMode) {
        this.permissionMode = permissionMode;
    }

    public Map<String, String> getEnv() {
        return env;
    }

    public void setEnv(Map<String, String> env) {
        this.env = env;
    }

    public Integer getMaxSessionTurns() {
        return maxSessionTurns;
    }

    public void setMaxSessionTurns(Integer maxSessionTurns) {
        this.maxSessionTurns = maxSessionTurns;
    }

    public List<String> getCoreTools() {
        return coreTools;
    }

    public void setCoreTools(List<String> coreTools) {
        this.coreTools = coreTools;
    }

    public List<String> getExcludeTools() {
        return excludeTools;
    }

    public void setExcludeTools(List<String> excludeTools) {
        this.excludeTools = excludeTools;
    }

    public List<String> getAllowedTools() {
        return allowedTools;
    }

    public void setAllowedTools(List<String> allowedTools) {
        this.allowedTools = allowedTools;
    }

    public String getAuthType() {
        return authType;
    }

    public void setAuthType(String authType) {
        this.authType = authType;
    }

    public Boolean getIncludePartialMessages() {
        return includePartialMessages;
    }

    public void setIncludePartialMessages(Boolean includePartialMessages) {
        this.includePartialMessages = includePartialMessages;
    }

    public Long getTurnTimeoutMs() {
        return turnTimeoutMs;
    }

    public void setTurnTimeoutMs(Long turnTimeoutMs) {
        this.turnTimeoutMs = turnTimeoutMs;
    }

    public Long getMessageTimeoutMs() {
        return messageTimeoutMs;
    }

    public void setMessageTimeoutMs(Long messageTimeoutMs) {
        this.messageTimeoutMs = messageTimeoutMs;
    }

    @Override
    public TransportOptions clone() {
        try {
            return (TransportOptions) super.clone();
        } catch (CloneNotSupportedException e) {
            throw new AssertionError();
        }
    }
}

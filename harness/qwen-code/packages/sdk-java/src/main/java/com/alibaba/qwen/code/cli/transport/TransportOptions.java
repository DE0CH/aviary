package com.alibaba.qwen.code.cli.transport;

import java.util.List;
import java.util.Map;

import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import com.alibaba.qwen.code.cli.utils.Timeout;

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
    private Boolean skillsEnable;
    private Timeout turnTimeout;
    private Timeout messageTimeout;
    private String resumeSessionId;
    private List<String> otherOptions;

    public String getPathToQwenExecutable() {
        return pathToQwenExecutable;
    }

    public TransportOptions setPathToQwenExecutable(String pathToQwenExecutable) {
        this.pathToQwenExecutable = pathToQwenExecutable;
        return this;
    }

    public String getCwd() {
        return cwd;
    }

    public TransportOptions setCwd(String cwd) {
        this.cwd = cwd;
        return this;
    }

    public String getModel() {
        return model;
    }

    public TransportOptions setModel(String model) {
        this.model = model;
        return this;
    }

    public PermissionMode getPermissionMode() {
        return permissionMode;
    }

    public TransportOptions setPermissionMode(PermissionMode permissionMode) {
        this.permissionMode = permissionMode;
        return this;
    }

    public Map<String, String> getEnv() {
        return env;
    }

    public TransportOptions setEnv(Map<String, String> env) {
        this.env = env;
        return this;
    }

    public Integer getMaxSessionTurns() {
        return maxSessionTurns;
    }

    public TransportOptions setMaxSessionTurns(Integer maxSessionTurns) {
        this.maxSessionTurns = maxSessionTurns;
        return this;
    }

    public List<String> getCoreTools() {
        return coreTools;
    }

    public TransportOptions setCoreTools(List<String> coreTools) {
        this.coreTools = coreTools;
        return this;
    }

    public List<String> getExcludeTools() {
        return excludeTools;
    }

    public TransportOptions setExcludeTools(List<String> excludeTools) {
        this.excludeTools = excludeTools;
        return this;
    }

    public List<String> getAllowedTools() {
        return allowedTools;
    }

    public TransportOptions setAllowedTools(List<String> allowedTools) {
        this.allowedTools = allowedTools;
        return this;
    }

    public String getAuthType() {
        return authType;
    }

    public TransportOptions setAuthType(String authType) {
        this.authType = authType;
        return this;
    }

    public Boolean getIncludePartialMessages() {
        return includePartialMessages;
    }

    public TransportOptions setIncludePartialMessages(Boolean includePartialMessages) {
        this.includePartialMessages = includePartialMessages;
        return this;
    }

    public Boolean getSkillsEnable() {
        return skillsEnable;
    }

    public TransportOptions setSkillsEnable(Boolean skillsEnable) {
        this.skillsEnable = skillsEnable;
        return this;
    }

    public Timeout getTurnTimeout() {
        return turnTimeout;
    }

    public TransportOptions setTurnTimeout(Timeout turnTimeout) {
        this.turnTimeout = turnTimeout;
        return this;
    }

    public Timeout getMessageTimeout() {
        return messageTimeout;
    }

    public TransportOptions setMessageTimeout(Timeout messageTimeout) {
        this.messageTimeout = messageTimeout;
        return this;
    }

    public String getResumeSessionId() {
        return resumeSessionId;
    }

    public TransportOptions setResumeSessionId(String resumeSessionId) {
        this.resumeSessionId = resumeSessionId;
        return this;
    }

    public List<String> getOtherOptions() {
        return otherOptions;
    }

    public TransportOptions setOtherOptions(List<String> otherOptions) {
        this.otherOptions = otherOptions;
        return this;
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

package com.alibaba.qwen.code.cli.protocol.message;

import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "type", typeName = "user")
public class SDKUserMessage extends MessageBase {
    private String uuid;

    @JSONField(name = "session_id")
    private String sessionId;
    private final APIUserMessage message = new APIUserMessage();

    @JSONField(name = "parent_tool_use_id")
    private String parentToolUseId;
    private Map<String, String> options;

    public SDKUserMessage() {
        super();
        this.setType("user");
    }

    public String getUuid() {
        return uuid;
    }

    public void setUuid(String uuid) {
        this.uuid = uuid;
    }

    public String getSessionId() {
        return sessionId;
    }

    public SDKUserMessage setSessionId(String sessionId) {
        this.sessionId = sessionId;
        return this;
    }

    public SDKUserMessage setContent(String content) {
        message.setContent(content);
        return this;
    }

    public String getContent() {
        return message.getContent();
    }

    public String getParentToolUseId() {
        return parentToolUseId;
    }

    public SDKUserMessage setParentToolUseId(String parentToolUseId) {
        this.parentToolUseId = parentToolUseId;
        return this;
    }

    public Map<String, String> getOptions() {
        return options;
    }

    public SDKUserMessage setOptions(Map<String, String> options) {
        this.options = options;
        return this;
    }

    public static class APIUserMessage {
        private String role = "user";
        private String content;

        // Getters and Setters
        public String getRole() {
            return role;
        }

        public void setRole(String role) {
            this.role = role;
        }

        public String getContent() {
            return content;
        }

        public void setContent(String content) {
            this.content = content;
        }
    }
}

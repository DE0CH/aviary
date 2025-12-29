package com.alibaba.qwen.code.cli.protocol.message.assistant;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.message.MessageBase;

@JSONType(typeKey = "type", typeName = "assistant")
public class SDKAssistantMessage extends MessageBase {
    private String uuid;

    @JSONField(name = "session_id")
    private String sessionId;
    private APIAssistantMessage message;

    @JSONField(name = "parent_tool_use_id")
    private String parentToolUseId;

    public String getUuid() {
        return uuid;
    }

    public void setUuid(String uuid) {
        this.uuid = uuid;
    }

    public String getSessionId() {
        return sessionId;
    }

    public void setSessionId(String sessionId) {
        this.sessionId = sessionId;
    }

    public APIAssistantMessage getMessage() {
        return message;
    }

    public void setMessage(APIAssistantMessage message) {
        this.message = message;
    }

    public String getParentToolUseId() {
        return parentToolUseId;
    }

    public void setParentToolUseId(String parentToolUseId) {
        this.parentToolUseId = parentToolUseId;
    }
}

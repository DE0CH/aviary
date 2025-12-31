package com.alibaba.qwen.code.cli.protocol.message.assistant;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.message.MessageBase;
import com.alibaba.qwen.code.cli.protocol.message.assistant.event.StreamEvent;

@JSONType(typeKey = "type", typeName = "stream_event")
public class SDKPartialAssistantMessage extends MessageBase {
    private String uuid;

    @JSONField(name = "session_id")
    private String sessionId;
    private StreamEvent event;

    @JSONField(name = "parent_tool_use_id")
    private String parentToolUseId;

    public SDKPartialAssistantMessage() {
        super();
        this.type = "stream_event";
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

    public void setSessionId(String sessionId) {
        this.sessionId = sessionId;
    }

    public StreamEvent getEvent() {
        return event;
    }

    public void setEvent(StreamEvent event) {
        this.event = event;
    }

    public String getParentToolUseId() {
        return parentToolUseId;
    }

    public void setParentToolUseId(String parentToolUseId) {
        this.parentToolUseId = parentToolUseId;
    }
}

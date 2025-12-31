package com.alibaba.qwen.code.cli.protocol.message.assistant.event;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "type", typeName = "StreamEvent",
        seeAlso = {MessageStartStreamEvent.class, MessageStopStreamEvent.class, ContentBlockStartEvent.class, ContentBlockStopEvent.class,
                ContentBlockDeltaEvent.class})
public class StreamEvent {
    protected String type;

    public String getType() {
        return type;
    }

    public void setType(String type) {
        this.type = type;
    }
}

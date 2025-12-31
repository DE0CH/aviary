package com.alibaba.qwen.code.cli.protocol.message.assistant.block;

import java.util.List;

import com.alibaba.fastjson2.JSON;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;

@JSONType(typeKey = "type", typeName = "ContentBlock", seeAlso = { TextBlock.class, ToolResultBlock.class, ThinkingBlock.class, ToolUseBlock.class })
public abstract class ContentBlock implements AssistantContent {
    protected String type;
    protected List<Annotation> annotations;

    public String getType() {
        return type;
    }

    public void setType(String type) {
        this.type = type;
    }

    public List<Annotation> getAnnotations() {
        return annotations;
    }

    public void setAnnotations(List<Annotation> annotations) {
        this.annotations = annotations;
    }

    public String toString() {
        return JSON.toJSONString(this);
    }
}

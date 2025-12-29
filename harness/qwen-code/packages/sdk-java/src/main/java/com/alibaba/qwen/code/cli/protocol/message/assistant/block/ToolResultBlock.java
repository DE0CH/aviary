package com.alibaba.qwen.code.cli.protocol.message.assistant.block;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "type", typeName = "tool_result")
public class ToolResultBlock extends ContentBlock {
    @JSONField(name = "tool_use_id")
    private String toolUseId;

    @JSONField(name = "content")
    private Object content; // Can be String or List<ContentBlock>

    @JSONField(name = "is_error")
    private Boolean isError;

    public String getToolUseId() {
        return toolUseId;
    }

    public void setToolUseId(String toolUseId) {
        this.toolUseId = toolUseId;
    }

    public Object getContent() {
        return content;
    }

    public void setContent(Object content) {
        this.content = content;
    }

    public Boolean getIsError() {
        return isError;
    }

    public void setIsError(Boolean isError) {
        this.isError = isError;
    }
}

package com.alibaba.qwen.code.cli.protocol.data;

import com.alibaba.fastjson2.annotation.JSONField;

public class CLIPermissionDenial {
    @JSONField(name = "tool_name")
    private String toolName;

    @JSONField(name = "tool_use_id")
    private String toolUseId;

    @JSONField(name = "tool_input")
    private Object toolInput;

    public String getToolName() {
        return toolName;
    }

    public void setToolName(String toolName) {
        this.toolName = toolName;
    }

    public String getToolUseId() {
        return toolUseId;
    }

    public void setToolUseId(String toolUseId) {
        this.toolUseId = toolUseId;
    }

    public Object getToolInput() {
        return toolInput;
    }

    public void setToolInput(Object toolInput) {
        this.toolInput = toolInput;
    }
}

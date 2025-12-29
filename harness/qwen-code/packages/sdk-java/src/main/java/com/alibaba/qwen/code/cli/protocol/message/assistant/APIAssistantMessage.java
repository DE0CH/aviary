package com.alibaba.qwen.code.cli.protocol.message.assistant;

import java.util.List;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.qwen.code.cli.protocol.data.Usage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.block.ContentBlock;

public class APIAssistantMessage {
    private String id;
    private String type = "message";
    private String role = "assistant";
    private String model;
    private List<ContentBlock> content;

    @JSONField(name = "stop_reason")
    private String stopReason;
    private Usage usage;

    // Getters and setters
    public String getId() {
        return id;
    }

    public void setId(String id) {
        this.id = id;
    }

    public String getType() {
        return type;
    }

    public void setType(String type) {
        this.type = type;
    }

    public String getRole() {
        return role;
    }

    public void setRole(String role) {
        this.role = role;
    }

    public String getModel() {
        return model;
    }

    public void setModel(String model) {
        this.model = model;
    }

    public String getStopReason() {
        return stopReason;
    }

    public void setStopReason(String stopReason) {
        this.stopReason = stopReason;
    }

    public Usage getUsage() {
        return usage;
    }

    public void setUsage(Usage usage) {
        this.usage = usage;
    }

    public List<ContentBlock> getContent() {
        return content;
    }

    public void setContent(List<ContentBlock> content) {
        this.content = content;
    }
}

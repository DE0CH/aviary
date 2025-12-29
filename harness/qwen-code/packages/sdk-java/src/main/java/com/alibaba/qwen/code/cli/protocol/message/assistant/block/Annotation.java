package com.alibaba.qwen.code.cli.protocol.message.assistant.block;

import com.alibaba.fastjson2.annotation.JSONField;

public class Annotation {
    @JSONField(name = "type")
    private String type;

    @JSONField(name = "value")
    private String value;

    // Getters and setters
    public String getType() {
        return type;
    }

    public void setType(String type) {
        this.type = type;
    }

    public String getValue() {
        return value;
    }

    public void setValue(String value) {
        this.value = value;
    }
}

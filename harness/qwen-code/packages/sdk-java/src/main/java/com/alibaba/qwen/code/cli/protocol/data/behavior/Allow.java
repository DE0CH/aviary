package com.alibaba.qwen.code.cli.protocol.data.behavior;

import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "operation", typeName = "allow")
public class Allow extends Behavior {
    public Allow() {
        super();
        this.behavior = Operation.allow;
    }
    Map<String, Object> updatedInput;

    public Map<String, Object> getUpdatedInput() {
        return updatedInput;
    }

    public Allow setUpdatedInput(Map<String, Object> updatedInput) {
        this.updatedInput = updatedInput;
        return this;
    }
}

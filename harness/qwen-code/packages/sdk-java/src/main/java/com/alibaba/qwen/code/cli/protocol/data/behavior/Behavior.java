package com.alibaba.qwen.code.cli.protocol.data.behavior;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "operation", typeName = "Behavior", seeAlso = {Allow.class, Deny.class})
public class Behavior {
    Operation behavior;

    public Operation getBehavior() {
        return behavior;
    }

    public void setBehavior(Operation behavior) {
        this.behavior = behavior;
    }

    public enum Operation {
        allow,
        deny
    }

    public static Behavior defaultBehavior() {
        return new Deny().setMessage("Default Behavior Permission denied");
    }
}

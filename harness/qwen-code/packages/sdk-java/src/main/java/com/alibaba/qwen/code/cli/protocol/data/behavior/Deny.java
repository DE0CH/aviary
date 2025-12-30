package com.alibaba.qwen.code.cli.protocol.data.behavior;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "operation", typeName = "deny")
public class Deny extends Behavior {
    public Deny() {
        super();
        this.behavior = Operation.deny;
    }

    String message;

    public String getMessage() {
        return message;
    }

    public Deny setMessage(String message) {
        this.message = message;
        return this;
    }
}

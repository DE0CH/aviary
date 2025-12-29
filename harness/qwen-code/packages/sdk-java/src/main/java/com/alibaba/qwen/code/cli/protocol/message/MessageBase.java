package com.alibaba.qwen.code.cli.protocol.message;

import com.alibaba.fastjson2.JSON;
import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(alphabetic = false, typeKey = "type", typeName = "MessageBase")
public class MessageBase implements Message{
    protected String type;

    public String toString() {
        return JSON.toJSONString(this);
    }

    @Override
    public String getType() {
        return type;
    }

    public void setType(String type) {
        this.type = type;
    }
}

package com.alibaba.qwen.code.cli.protocol.message.control;

public class CLIControlSetModelRequest {
    String subtype = "set_model";
    String model;

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public String getModel() {
        return model;
    }

    public void setModel(String model) {
        this.model = model;
    }
}

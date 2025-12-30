package com.alibaba.qwen.code.cli.protocol.message.control;

public class CLIControlInterruptRequest {
    String subtype = "interrupt";

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }
}

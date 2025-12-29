package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.qwen.code.cli.protocol.data.Capabilities;

public class CLIControlInitializeResponse {
    String subtype = "initialize";
    Capabilities capabilities;

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public Capabilities getCapabilities() {
        return capabilities;
    }

    public void setCapabilities(Capabilities capabilities) {
        this.capabilities = capabilities;
    }
}

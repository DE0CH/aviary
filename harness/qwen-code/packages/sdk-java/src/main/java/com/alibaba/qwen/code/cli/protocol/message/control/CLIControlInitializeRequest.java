package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.qwen.code.cli.protocol.data.InitializeConfig;

public class CLIControlInitializeRequest {
    String subtype = "initialize";

    @JSONField(unwrapped = true)
    InitializeConfig initializeConfig = new InitializeConfig();

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public InitializeConfig getInitializeConfig() {
        return initializeConfig;
    }

    public CLIControlInitializeRequest setInitializeConfig(InitializeConfig initializeConfig) {
        this.initializeConfig = initializeConfig;
        return this;
    }
}

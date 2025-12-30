package com.alibaba.qwen.code.cli.protocol.message.control;

public class CLIControlSetPermissionModeRequest {
    String subtype = "set_permission_mode";

    String mode;

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public String getMode() {
        return mode;
    }

    public void setMode(String mode) {
        this.mode = mode;
    }
}

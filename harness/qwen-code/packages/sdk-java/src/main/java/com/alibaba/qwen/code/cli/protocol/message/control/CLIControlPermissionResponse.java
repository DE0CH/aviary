package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;

public class CLIControlPermissionResponse {
    private String subtype = "can_use_tool";

    @JSONField(unwrapped = true)
    Behavior behavior;

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public Behavior getBehavior() {
        return behavior;
    }

    public CLIControlPermissionResponse setBehavior(Behavior behavior) {
        this.behavior = behavior;
        return this;
    }
}

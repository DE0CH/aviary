package com.alibaba.qwen.code.cli.protocol.data;

import com.alibaba.fastjson2.annotation.JSONField;

public class Capabilities {
    @JSONField(name = "can_handle_can_use_tool")
    boolean canHandleCanUseTool;

    @JSONField(name = "can_handle_hook_callback")
    boolean canHandleHookCallback;

    @JSONField(name = "can_set_permission_mode")
    boolean canSetPermissionMode;

    @JSONField(name = "can_set_model")
    boolean canSetModel;

    @JSONField(name = "can_handle_mcp_message")
    boolean canHandleMcpMessage;

    public boolean isCanHandleCanUseTool() {
        return canHandleCanUseTool;
    }

    public void setCanHandleCanUseTool(boolean canHandleCanUseTool) {
        this.canHandleCanUseTool = canHandleCanUseTool;
    }

    public boolean isCanHandleHookCallback() {
        return canHandleHookCallback;
    }

    public void setCanHandleHookCallback(boolean canHandleHookCallback) {
        this.canHandleHookCallback = canHandleHookCallback;
    }

    public boolean isCanSetPermissionMode() {
        return canSetPermissionMode;
    }

    public void setCanSetPermissionMode(boolean canSetPermissionMode) {
        this.canSetPermissionMode = canSetPermissionMode;
    }

    public boolean isCanSetModel() {
        return canSetModel;
    }

    public void setCanSetModel(boolean canSetModel) {
        this.canSetModel = canSetModel;
    }

    public boolean isCanHandleMcpMessage() {
        return canHandleMcpMessage;
    }

    public void setCanHandleMcpMessage(boolean canHandleMcpMessage) {
        this.canHandleMcpMessage = canHandleMcpMessage;
    }
}

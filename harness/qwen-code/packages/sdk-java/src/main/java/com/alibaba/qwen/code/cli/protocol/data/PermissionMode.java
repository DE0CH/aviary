package com.alibaba.qwen.code.cli.protocol.data;

public enum PermissionMode {
    DEFAULT("default"),
    PLAN("plan"),
    AUTO_EDIT("auto-edit"),
    YOLO("yolo");

    private final String value;

    PermissionMode(String value) {
        this.value = value;
    }

    public String getValue() {
        return value;
    }

    public static PermissionMode fromValue(String value) {
        for (PermissionMode mode : PermissionMode.values()) {
            if (mode.value.equals(value)) {
                return mode;
            }
        }
        throw new IllegalArgumentException("Unknown permission mode: " + value);
    }
}

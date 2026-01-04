package com.alibaba.qwen.code.cli.protocol.data.behavior;

import com.alibaba.fastjson2.annotation.JSONType;

/**
 * Base class for behavior objects that define how the CLI should handle requests.
 */
@JSONType(typeKey = "operation", typeName = "Behavior", seeAlso = {Allow.class, Deny.class})
public class Behavior {
    /**
     * The behavior operation (allow or deny).
     */
    Operation behavior;

    /**
     * Gets the behavior operation.
     *
     * @return The behavior operation
     */
    public Operation getBehavior() {
        return behavior;
    }

    /**
     * Sets the behavior operation.
     *
     * @param behavior The behavior operation
     */
    public void setBehavior(Operation behavior) {
        this.behavior = behavior;
    }

    /**
     * Represents the type of operation.
     */
    public enum Operation {
        /**
         * Allow the operation.
         */
        allow,
        /**
         * Deny the operation.
         */
        deny
    }

    /**
     * Gets the default behavior (deny with message).
     *
     * @return The default behavior
     */
    public static Behavior defaultBehavior() {
        return new Deny().setMessage("Default Behavior Permission denied");
    }
}

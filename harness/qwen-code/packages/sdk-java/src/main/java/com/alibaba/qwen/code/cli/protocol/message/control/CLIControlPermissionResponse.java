package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;

/**
 * Represents a control permission response from the CLI.
 *
 * @author skyfire
 * @version $Id: 0.0.1
 */
public class CLIControlPermissionResponse {
    /**
     * The subtype of the response ("can_use_tool").
     */
    private String subtype = "can_use_tool";

    /**
     * The behavior for the permission request.
     */
    @JSONField(unwrapped = true)
    Behavior behavior;

    /**
     * Gets the subtype of the response.
     *
     * @return The subtype of the response
     */
    public String getSubtype() {
        return subtype;
    }

    /**
     * Sets the subtype of the response.
     *
     * @param subtype The subtype of the response
     */
    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    /**
     * Gets the behavior for the permission request.
     *
     * @return The behavior for the permission request
     */
    public Behavior getBehavior() {
        return behavior;
    }

    /**
     * Sets the behavior for the permission request.
     *
     * @param behavior The behavior for the permission request
     * @return This instance for method chaining
     */
    public CLIControlPermissionResponse setBehavior(Behavior behavior) {
        this.behavior = behavior;
        return this;
    }
}

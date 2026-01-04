package com.alibaba.qwen.code.cli.protocol.message.control;

/**
 * Represents a control request to set the permission mode in the CLI.
 */
public class CLIControlSetPermissionModeRequest {
    /**
     * The subtype of the request ("set_permission_mode").
     */
    String subtype = "set_permission_mode";

    /**
     * The permission mode to set.
     */
    String mode;

    /**
     * Gets the subtype of the request.
     *
     * @return The subtype of the request
     */
    public String getSubtype() {
        return subtype;
    }

    /**
     * Sets the subtype of the request.
     *
     * @param subtype The subtype of the request
     */
    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    /**
     * Gets the permission mode to set.
     *
     * @return The permission mode to set
     */
    public String getMode() {
        return mode;
    }

    /**
     * Sets the permission mode to set.
     *
     * @param mode The permission mode to set
     */
    public void setMode(String mode) {
        this.mode = mode;
    }
}

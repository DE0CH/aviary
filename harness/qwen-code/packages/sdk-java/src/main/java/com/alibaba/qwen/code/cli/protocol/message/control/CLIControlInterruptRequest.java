package com.alibaba.qwen.code.cli.protocol.message.control;

/**
 * Represents a control interrupt request to the CLI.
 *
 * @author skyfire
 * @version $Id: 0.0.1
 */
public class CLIControlInterruptRequest {
    /**
     * The subtype of the request ("interrupt").
     */
    String subtype = "interrupt";

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
}

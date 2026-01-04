package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.qwen.code.cli.protocol.data.Capabilities;

/**
 * Represents a control initialize response from the CLI.
 *
 * @author skyfire
 * @version $Id: 0.0.1
 */
public class CLIControlInitializeResponse {
    /**
     * The subtype of the response.
     */
    String subtype = "initialize";
    /**
     * The capabilities' information.
     */
    Capabilities capabilities;

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
     * Gets the capabilities information.
     *
     * @return The capabilities information
     */
    public Capabilities getCapabilities() {
        return capabilities;
    }

    /**
     * Sets the capabilities information.
     *
     * @param capabilities The capabilities information
     */
    public void setCapabilities(Capabilities capabilities) {
        this.capabilities = capabilities;
    }
}

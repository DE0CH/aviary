package com.alibaba.qwen.code.cli.protocol.message.control;

/**
 * Represents a control request to set the model in the CLI.
 */
public class CLIControlSetModelRequest {
    /**
     * The subtype of the request ("set_model").
     */
    String subtype = "set_model";
    /**
     * The model to set.
     */
    String model;

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
     * Gets the model to set.
     *
     * @return The model to set
     */
    public String getModel() {
        return model;
    }

    /**
     * Sets the model to set.
     *
     * @param model The model to set
     */
    public void setModel(String model) {
        this.model = model;
    }
}

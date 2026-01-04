package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.qwen.code.cli.protocol.data.InitializeConfig;

/**
 * Represents a control initialize request to the CLI.
 */
public class CLIControlInitializeRequest {
    /**
     * The subtype of the request.
     */
    String subtype = "initialize";

    /**
     * The initialization configuration.
     */
    @JSONField(unwrapped = true)
    InitializeConfig initializeConfig = new InitializeConfig();

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
     * Gets the initialization configuration.
     *
     * @return The initialization configuration
     */
    public InitializeConfig getInitializeConfig() {
        return initializeConfig;
    }

    /**
     * Sets the initialization configuration.
     *
     * @param initializeConfig The initialization configuration
     * @return This instance for method chaining
     */
    public CLIControlInitializeRequest setInitializeConfig(InitializeConfig initializeConfig) {
        this.initializeConfig = initializeConfig;
        return this;
    }
}

package com.alibaba.qwen.code.cli.transport;

import java.io.IOException;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeoutException;
import java.util.function.Function;

/**
 * Defines the contract for communication with the Qwen Code CLI.
 */
public interface Transport {
    /**
     * Gets the transport options used by this transport.
     *
     * @return The transport options
     */
    TransportOptions getTransportOptions();

    /**
     * Checks if the transport is currently reading.
     *
     * @return true if reading, false otherwise
     */
    boolean isReading();

    /**
     * Starts the transport.
     *
     * @throws IOException if starting fails
     */
    void start() throws IOException;

    /**
     * Closes the transport and releases resources.
     *
     * @throws IOException if closing fails
     */
    void close() throws IOException;

    /**
     * Checks if the transport is available for communication.
     *
     * @return true if available, false otherwise
     */
    boolean isAvailable();

    /**
     * Sends a message and waits for a single-line response.
     *
     * @param message The message to send
     * @return The response message
     * @throws IOException if an I/O error occurs
     * @throws ExecutionException if an execution error occurs
     * @throws InterruptedException if the operation is interrupted
     * @throws TimeoutException if the operation times out
     */
    String inputWaitForOneLine(String message) throws IOException, ExecutionException, InterruptedException, TimeoutException;

    /**
     * Sends a message and waits for a multi-line response.
     *
     * @param message The message to send
     * @param callBackFunction A function to process each line of the response
     * @throws IOException if an I/O error occurs
     */
    void inputWaitForMultiLine(String message, Function<String, Boolean> callBackFunction) throws IOException;

    /**
     * Sends a message without waiting for a response.
     *
     * @param message The message to send
     * @throws IOException if an I/O error occurs
     */
    void inputNoWaitResponse(String message) throws IOException;
}

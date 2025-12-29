package com.alibaba.qwen.code.cli.transport.process;

import com.alibaba.qwen.code.cli.transport.Transport;
import com.alibaba.qwen.code.cli.transport.TransportOptions;

import org.apache.commons.lang3.exception.ContextedRuntimeException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.lang.ProcessBuilder.Redirect;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.function.Function;

public class ProcessTransport implements Transport {
    private static final Logger log = LoggerFactory.getLogger(ProcessTransport.class);
    TransportOptionsAdapter transportOptionsAdapter;

    protected final Long turnTimeoutMs;
    protected final Long messageTimeoutMs;

    protected Process process;
    protected BufferedWriter processInput;
    protected BufferedReader processOutput;
    protected BufferedReader processError;

    public ProcessTransport() throws IOException {
        this(new TransportOptions());
    }

    public ProcessTransport(TransportOptions transportOptions) throws IOException {
        this.transportOptionsAdapter = new TransportOptionsAdapter(transportOptions);
        turnTimeoutMs = transportOptionsAdapter.getHandledTransportOptions().getTurnTimeoutMs();
        messageTimeoutMs = transportOptionsAdapter.getHandledTransportOptions().getMessageTimeoutMs();
        start();
    }

    protected void start() throws IOException {
        String[] commandArgs = transportOptionsAdapter.buildCommandArgs();
        log.debug("trans to command args: {}", transportOptionsAdapter);

        ProcessBuilder processBuilder = new ProcessBuilder(commandArgs)
                .redirectOutput(Redirect.PIPE)
                .redirectInput(Redirect.PIPE)
                .redirectError(Redirect.PIPE)
                .redirectErrorStream(false)
                .directory(new File(transportOptionsAdapter.getCwd()));

        process = processBuilder.start();
        processInput = new BufferedWriter(new OutputStreamWriter(process.getOutputStream()));
        processOutput = new BufferedReader(new InputStreamReader(process.getInputStream()));
        processError = new BufferedReader(new InputStreamReader(process.getErrorStream()));
        startErrorReading();
    }

    @Override
    public void close() throws IOException {
        if (processInput != null) {
            processInput.close();
        }
        if (processOutput != null) {
            processOutput.close();
        }
        if (processError != null) {
            processError.close();
        }
        if (process != null) {
            process.destroy();
        }
    }

    @Override
    public boolean isAvailable() {
        return process != null && process.isAlive();
    }

    @Override
    public String inputWaitForOneLine(String message) throws IOException, ExecutionException, InterruptedException, TimeoutException {
        return inputWaitForOneLine(message, turnTimeoutMs);
    }

    private String inputWaitForOneLine(String message, long timeOutInMs)
            throws IOException, TimeoutException, InterruptedException, ExecutionException {
        inputNoWaitResponse(message);
        CompletableFuture<String> future = CompletableFuture.supplyAsync(() -> {
            try {
                return processOutput.readLine();
            } catch (IOException e) {
                throw new ContextedRuntimeException("read line error", e)
                        .addContextValue("message", message);
            }
        });

        try {
            String line = future.get(timeOutInMs, TimeUnit.MILLISECONDS);
            log.info("inputWaitForOneLine result: {}", line);
            return line;
        } catch (TimeoutException e) {
            future.cancel(true);
            log.warn("read message timeout {}, canceled readOneLine task", timeOutInMs, e);
            throw e;
        } catch (InterruptedException e) {
            future.cancel(true);
            log.warn("interrupted task, canceled task", e);
            throw e;
        } catch (ExecutionException e) {
            future.cancel(true);
            log.warn("the readOneLine task execute error", e);
            throw e;
        }
    }

    @Override
    public void inputWaitForMultiLine(String message, Function<String, Boolean> callBackFunction) throws IOException {
        inputWaitForMultiLine(message, callBackFunction, turnTimeoutMs);
    }

    private void inputWaitForMultiLine(String message, Function<String, Boolean> callBackFunction, long timeOutInMs) throws IOException {
        log.debug("input message for multiLine: {}", message);
        inputNoWaitResponse(message);

        CompletableFuture<Void> future = CompletableFuture.runAsync(() -> iterateOutput(callBackFunction));
        try {
            future.get(timeOutInMs, TimeUnit.MILLISECONDS);
        } catch (TimeoutException e) {
            future.cancel(true);
            log.warn("read message timeout {}, canceled readMultiMessages task", timeOutInMs, e);
        } catch (InterruptedException e) {
            future.cancel(true);
            log.warn("interrupted task, canceled task", e);
        } catch (ExecutionException e) {
            future.cancel(true);
            log.warn("the readMultiMessages task execute error", e);
        } catch (Exception e) {
            future.cancel(true);
            log.warn("other  error");
        }
    }

    @Override
    public void inputNoWaitResponse(String message) throws IOException {
        log.debug("input message to agent: {}", message);
        processInput.write(message);
        processInput.newLine();
        processInput.flush();
    }

    private void startErrorReading() {
        CompletableFuture.runAsync(() -> {
            try {
                String line;
                while ((line = processError.readLine()) != null) {
                    System.err.println("错误: " + line);
                }
            } catch (Exception e) {
                System.err.println("错误: " + e.getMessage());
            }
        });
    }

    private void iterateOutput(Function<String, Boolean> callBackFunction) {
        CompletableFuture<Void> future = CompletableFuture.runAsync(() -> {
            try {
                for (String line = processOutput.readLine(); line != null; line = processOutput.readLine()) {
                    log.debug("read a message from agent {}", line);
                    if (callBackFunction.apply(line)) {
                        break;
                    }
                }
            } catch (IOException e) {
                throw new RuntimeException("read process output error", e);
            }
        });

        try {
            future.get(messageTimeoutMs, TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            log.warn("read message task interrupted", e);
            future.cancel(true);
        } catch (TimeoutException e) {
            log.warn("Operation timed out", e);
            future.cancel(true);
        } catch (Exception e) {
            future.cancel(true);
            log.warn("Operation error", e);
        }
    }
}

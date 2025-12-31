package com.alibaba.qwen.code.cli.utils;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeoutException;
import java.util.function.BiConsumer;
import java.util.function.Supplier;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class MyConcurrentUtils {
    private static final Logger log = LoggerFactory.getLogger(MyConcurrentUtils.class);

    public static void runAndWait(Runnable runnable, Timeout timeOut) {
        CompletableFuture<Void> future = CompletableFuture.runAsync(() -> {
            try {
                runnable.run();
            } catch (Exception e) {
                throw new RuntimeException(e);
            }
        }, ThreadPoolConfig.getExecutor());
        try {
            future.get(timeOut.getValue(), timeOut.getUnit());
        } catch (InterruptedException e) {
            log.warn("task interrupted", e);
            future.cancel(true);
        } catch (TimeoutException e) {
            log.warn("Operation timed out", e);
            future.cancel(true);
        } catch (Exception e) {
            future.cancel(true);
            log.warn("Operation error", e);
        }
    }

    public static <T> T runAndWait(Supplier<T> supplier, Timeout timeOut)
            throws ExecutionException, InterruptedException, TimeoutException {
        CompletableFuture<T> future = CompletableFuture.supplyAsync(() -> {
            try {
                return supplier.get();
            } catch (Exception e) {
                throw new RuntimeException(e);
            }
        }, ThreadPoolConfig.getExecutor());

        try {
            return future.get(timeOut.getValue(), timeOut.getUnit());
        } catch (TimeoutException | InterruptedException | ExecutionException e) {
            future.cancel(true);
            throw e;
        }
    }

    public static void asyncRun(Runnable runnable, BiConsumer<Void, Throwable> errorCallback) {
        CompletableFuture<Void> future = CompletableFuture.runAsync(() -> {
            try {
                runnable.run();
            } catch (Exception e) {
                log.warn("async task error", e);
            }
        }, ThreadPoolConfig.getExecutor());
        future.whenComplete(errorCallback);
    }
}

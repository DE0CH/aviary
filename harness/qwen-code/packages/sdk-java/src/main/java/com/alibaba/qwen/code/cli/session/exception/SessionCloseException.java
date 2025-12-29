package com.alibaba.qwen.code.cli.session.exception;

public class SessionCloseException extends Exception {
    public SessionCloseException() {
    }

    public SessionCloseException(String message) {
        super(message);
    }

    public SessionCloseException(String message, Throwable cause) {
        super(message, cause);
    }

    public SessionCloseException(Throwable cause) {
        super(cause);
    }

    public SessionCloseException(String message, Throwable cause, boolean enableSuppression, boolean writableStackTrace) {
        super(message, cause, enableSuppression, writableStackTrace);
    }
}

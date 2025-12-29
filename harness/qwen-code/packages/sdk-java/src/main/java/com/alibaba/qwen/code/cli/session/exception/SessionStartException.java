package com.alibaba.qwen.code.cli.session.exception;

public class SessionStartException extends Exception {
    public SessionStartException() {
    }

    public SessionStartException(String message) {
        super(message);
    }

    public SessionStartException(String message, Throwable cause) {
        super(message, cause);
    }

    public SessionStartException(Throwable cause) {
        super(cause);
    }

    public SessionStartException(String message, Throwable cause, boolean enableSuppression, boolean writableStackTrace) {
        super(message, cause, enableSuppression, writableStackTrace);
    }
}

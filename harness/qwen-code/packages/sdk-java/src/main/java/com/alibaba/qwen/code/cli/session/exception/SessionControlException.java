package com.alibaba.qwen.code.cli.session.exception;

public class SessionControlException extends Exception {
    public SessionControlException() {
    }

    public SessionControlException(String message) {
        super(message);
    }

    public SessionControlException(String message, Throwable cause) {
        super(message, cause);
    }

    public SessionControlException(Throwable cause) {
        super(cause);
    }

    public SessionControlException(String message, Throwable cause, boolean enableSuppression, boolean writableStackTrace) {
        super(message, cause, enableSuppression, writableStackTrace);
    }
}

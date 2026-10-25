package com.accessor.jdbc;

/**
 * Rewrites JDBC's positional placeholders {@code ?} as the named parameters {@code @p1, @p2, ...}
 * that ASQL understands. Question marks inside text, bracketed names, {@code #date#} literals and
 * comments are left alone.
 */
final class SqlTranslator {
    /** The rewritten SQL and the number of placeholders found. */
    record Result(String sql, int parameters) {}

    private SqlTranslator() {}

    static Result translate(String sql) {
        StringBuilder out = new StringBuilder(sql.length() + 8);
        int n = 0;
        int i = 0;
        int len = sql.length();
        while (i < len) {
            char c = sql.charAt(i);
            switch (c) {
                case '\'', '"' -> i = copyQuoted(sql, i, c, out);
                case '[' -> i = copyUntil(sql, i, ']', out);
                case '#' -> i = copyUntil(sql, i, '#', out);
                case '-' -> {
                    if (i + 1 < len && sql.charAt(i + 1) == '-') {
                        int end = sql.indexOf('\n', i);
                        end = end < 0 ? len : end;
                        out.append(sql, i, end);
                        i = end;
                    } else {
                        out.append(c);
                        i++;
                    }
                }
                case '/' -> {
                    if (i + 1 < len && sql.charAt(i + 1) == '*') {
                        int end = sql.indexOf("*/", i + 2);
                        end = end < 0 ? len : end + 2;
                        out.append(sql, i, end);
                        i = end;
                    } else {
                        out.append(c);
                        i++;
                    }
                }
                case '?' -> {
                    n++;
                    out.append("@p").append(n);
                    i++;
                }
                default -> {
                    out.append(c);
                    i++;
                }
            }
        }
        return new Result(out.toString(), n);
    }

    /** Copies a quoted text, where a doubled quote is a quote inside it. */
    private static int copyQuoted(String sql, int start, char quote, StringBuilder out) {
        int i = start + 1;
        out.append(quote);
        while (i < sql.length()) {
            char c = sql.charAt(i);
            out.append(c);
            i++;
            if (c == quote) {
                if (i < sql.length() && sql.charAt(i) == quote) {
                    out.append(quote);
                    i++;
                } else {
                    break;
                }
            }
        }
        return i;
    }

    private static int copyUntil(String sql, int start, char end, StringBuilder out) {
        int stop = sql.indexOf(end, start + 1);
        stop = stop < 0 ? sql.length() : stop + 1;
        out.append(sql, start, stop);
        return stop;
    }
}

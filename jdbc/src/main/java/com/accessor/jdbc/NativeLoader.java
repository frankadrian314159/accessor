package com.accessor.jdbc;

import java.io.IOException;
import java.io.InputStream;
import java.lang.foreign.Arena;
import java.lang.foreign.SymbolLookup;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * Finds and loads the native Accessor library ({@code accessor_capi}).
 *
 * <p>In order:
 * <ol>
 *   <li>the file named by the system property {@code accessor.library} or the environment variable
 *       {@code ACCESSOR_LIBRARY};</li>
 *   <li>the copy inside this jar, {@code /native/<os>-<arch>/<file>}, which is extracted to a
 *       temporary file;</li>
 *   <li>the library search path of the operating system ({@code PATH}, {@code LD_LIBRARY_PATH},
 *       {@code DYLD_LIBRARY_PATH}).</li>
 * </ol>
 */
final class NativeLoader {
    private NativeLoader() {}

    static String os() {
        String n = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (n.contains("win")) {
            return "windows";
        }
        if (n.contains("mac") || n.contains("darwin")) {
            return "macos";
        }
        return "linux";
    }

    static String arch() {
        String a = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        return a.equals("amd64") || a.equals("x86_64") ? "x86_64" : a.equals("aarch64") || a.equals("arm64") ? "aarch64" : a;
    }

    static String fileName() {
        return switch (os()) {
            case "windows" -> "accessor_capi.dll";
            case "macos" -> "libaccessor_capi.dylib";
            default -> "libaccessor_capi.so";
        };
    }

    static SymbolLookup lookup() {
        List<String> tried = new ArrayList<>();
        String explicit = System.getProperty("accessor.library", System.getenv("ACCESSOR_LIBRARY"));
        if (explicit != null && !explicit.isBlank()) {
            Path p = Path.of(explicit);
            tried.add(p.toString());
            if (Files.isRegularFile(p)) {
                return SymbolLookup.libraryLookup(p, Arena.global());
            }
            throw new UnsatisfiedLinkError("accessor.library points to " + p + ", which is not a file");
        }
        String resource = "/native/" + os() + "-" + arch() + "/" + fileName();
        try (InputStream in = NativeLoader.class.getResourceAsStream(resource)) {
            if (in != null) {
                Path dir = Files.createTempDirectory("accessor-jdbc");
                Path file = dir.resolve(fileName());
                Files.copy(in, file, StandardCopyOption.REPLACE_EXISTING);
                file.toFile().deleteOnExit();
                dir.toFile().deleteOnExit();
                return SymbolLookup.libraryLookup(file, Arena.global());
            }
            tried.add("jar:" + resource);
        } catch (IOException e) {
            throw new UnsatisfiedLinkError("cannot extract the native library from the jar: " + e);
        }
        try {
            tried.add("system search path: " + fileName());
            return SymbolLookup.libraryLookup(fileName(), Arena.global());
        } catch (IllegalArgumentException e) {
            throw new UnsatisfiedLinkError(
                    "cannot find the Accessor native library (" + fileName() + "). Tried: " + String.join("; ", tried)
                            + ". Set -Daccessor.library=/path/to/" + fileName() + " or the ACCESSOR_LIBRARY environment variable.");
        }
    }
}

/*
 * A program with a deliberate heap overflow. run-c-tests.sh builds and runs it with the same flags as
 * the real tests and requires the checker to FAIL: it proves that Valgrind and AddressSanitizer are
 * really watching, so that a clean run of the real tests means something.
 */
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    char *p = malloc(8);
    memset(p, 'x', 8);
    volatile char c = p[8 + (argc > 99 ? 1 : 0)]; /* one byte past the end */
    (void)argv;
    free(p);
    return c == 'x' ? 0 : 1;
}

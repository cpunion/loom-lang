#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    int64_t x;
    int64_t y;
    int64_t z;
} Point;

_Noreturn static void fail(const char *message) {
    fprintf(stderr, "%s\n", message);
    exit(2);
}

static int64_t number(const char *text) {
    char *end;
    errno = 0;
    intmax_t value = strtoimax(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || value < 0 || value > INT64_MAX) {
        fail("expected a nonnegative int64");
    }
    return (int64_t)value;
}

static int64_t advance(int64_t value) {
    return (value * 17 + 23) % 1000003;
}

static int64_t alternate(int64_t value) {
    return (value * 7 + 11) % 1000003;
}

static int64_t int_lcg(int64_t n, int64_t seed) {
    int64_t state = seed % 1000003;
    for (int64_t i = 0; i < n; ++i) {
        state = advance(state);
    }
    return state;
}

static int64_t fib_recursive(int64_t n) {
    return n < 2 ? n : fib_recursive(n - 1) + fib_recursive(n - 2);
}

static Point step(Point old, int64_t delta) {
    return (Point){
        (old.y + delta) % 1000003,
        (old.z + old.x) % 1000003,
        (old.x + old.y + old.z) % 1000003,
    };
}

static int64_t record_value(int64_t n, int64_t seed) {
    Point value = {seed % 97, seed % 193, seed % 389};
    for (int64_t i = 0; i < n; ++i) {
        value = step(value, i % 31);
    }
    return value.x + value.y + value.z;
}

static int64_t list_build_scan(int64_t n, int64_t seed) {
    int64_t *values = NULL;
    size_t length = 0;
    size_t capacity = 0;
    int64_t state = seed % 1000003;
    for (int64_t i = 0; i < n; ++i) {
        state = advance(state);
        if (length == capacity) {
            if (capacity > SIZE_MAX / sizeof(*values) / 2) {
                fail("list capacity overflow");
            }
            size_t next_capacity = capacity == 0 ? 8 : capacity * 2;
            int64_t *next = realloc(values, next_capacity * sizeof(*values));
            if (next == NULL) {
                free(values);
                fail("allocation failed");
            }
            values = next;
            capacity = next_capacity;
        }
        values[length++] = state;
    }
    int64_t sum = 0;
    for (size_t i = 0; i < length; ++i) {
        sum += values[i];
    }
    free(values);
    return sum;
}

static int64_t function_value(int64_t n, int64_t seed) {
    int64_t (*action)(int64_t) = seed % 2 == 0 ? advance : alternate;
    int64_t state = seed % 1000003;
    for (int64_t i = 0; i < n; ++i) {
        state = action(state);
    }
    return state;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fail("usage: benchmark CASE N SEED");
    }
    int64_t n = number(argv[2]);
    int64_t seed = number(argv[3]);
    int64_t checksum;
    if (strcmp(argv[1], "int_lcg") == 0) {
        checksum = int_lcg(n, seed);
    } else if (strcmp(argv[1], "fib_recursive") == 0) {
        checksum = fib_recursive(n);
    } else if (strcmp(argv[1], "record_value") == 0) {
        checksum = record_value(n, seed);
    } else if (strcmp(argv[1], "list_build_scan") == 0) {
        checksum = list_build_scan(n, seed);
    } else if (strcmp(argv[1], "function_value") == 0) {
        checksum = function_value(n, seed);
    } else {
        fail("unknown benchmark case");
    }
    printf("%" PRId64 "\n", checksum);
    return 0;
}

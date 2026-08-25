#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const int64_t MODULUS = INT64_C(2147483647);
static const int64_t MULTIPLIER = INT64_C(48271);

typedef struct {
  int64_t total;
  int64_t calls;
} Counter;

typedef struct {
  int64_t *data;
  size_t length;
  size_t capacity;
} IntList;

static void fail(const char *message) {
  fprintf(stderr, "%s\n", message);
  exit(2);
}

static int64_t modular_product(int64_t left, int64_t right) {
  int64_t product = left * right;
  return product - (product / MODULUS) * MODULUS;
}

static int64_t int_lcg(int64_t size) {
  int64_t state = 1;
  for (int64_t index = 0; index < size; ++index) {
    state = modular_product(state, MULTIPLIER);
  }
  return state;
}

static int64_t periodic_value(int64_t index) {
  return index - (index / 1024) * 1024;
}

static void counter_add(Counter *counter, int64_t value) {
  counter->total += value;
  counter->calls += 1;
}

static Counter record_method(int64_t size) {
  Counter counter = {0, 0};
  for (int64_t index = 0; index < size; ++index) {
    counter_add(&counter, periodic_value(index));
  }
  return counter;
}

static void list_add(IntList *list, int64_t value) {
  if (list->length == list->capacity) {
    size_t next_capacity = list->capacity == 0 ? 4 : list->capacity * 2;
    if (next_capacity < list->capacity ||
        next_capacity > SIZE_MAX / sizeof(int64_t)) {
      fail("list capacity overflow");
    }
    int64_t *next = realloc(list->data, next_capacity * sizeof(int64_t));
    if (next == NULL) {
      fail("list allocation failed");
    }
    list->data = next;
    list->capacity = next_capacity;
  }
  list->data[list->length++] = value;
}

static int64_t list_get(const IntList *list, size_t index) {
  if (index >= list->length) {
    fail("list index outside bounds");
  }
  return list->data[index];
}

static int64_t list_build_scan(int64_t size) {
  IntList values = {NULL, 0, 0};
  for (int64_t index = 0; index < size; ++index) {
    list_add(&values, periodic_value(index));
  }
  int64_t checksum = 0;
  for (int64_t index = 0; index < size; ++index) {
    checksum += list_get(&values, (size_t)index);
  }
  if (values.length != (size_t)size) {
    free(values.data);
    fail("list length mismatch");
  }
  free(values.data);
  return checksum;
}

static int64_t fibonacci(int64_t value) {
  if (value < 2) {
    return value;
  }
  return fibonacci(value - 1) + fibonacci(value - 2);
}

static int64_t parse_number(const char *text, const char *message) {
  errno = 0;
  char *end = NULL;
  intmax_t parsed = strtoimax(text, &end, 10);
  if (errno != 0 || end == text || *end != '\0' || parsed < 0 ||
      parsed > INT64_MAX) {
    fail(message);
  }
  return (int64_t)parsed;
}

static void run(const char *name, int64_t size, int64_t expected) {
  if (size < 0) {
    fail("size must be non-negative");
  }
  if (strcmp(name, "int_lcg") == 0) {
    if (size > 100000000) {
      fail("int_lcg size exceeds limit");
    }
    if (int_lcg(size) != expected) {
      fail("int_lcg checksum mismatch");
    }
  } else if (strcmp(name, "record_method") == 0) {
    if (size > 100000000) {
      fail("record_method size exceeds limit");
    }
    Counter value = record_method(size);
    if (value.total != expected || value.calls != size) {
      fail("record_method checksum mismatch");
    }
  } else if (strcmp(name, "list_build_scan") == 0) {
    if (size > 10000000) {
      fail("list_build_scan size exceeds limit");
    }
    if (list_build_scan(size) != expected) {
      fail("list_build_scan checksum mismatch");
    }
  } else if (strcmp(name, "fib_recursive") == 0) {
    if (size > 45) {
      fail("fib_recursive size exceeds limit");
    }
    if (fibonacci(size) != expected) {
      fail("fib_recursive checksum mismatch");
    }
  } else {
    fail("unknown benchmark case");
  }
}

int main(int argc, char **argv) {
  if (argc != 4) {
    fail("usage: benchmark CASE SIZE EXPECTED");
  }
  int64_t size = parse_number(argv[2], "invalid benchmark size");
  int64_t expected = parse_number(argv[3], "invalid expected checksum");
  run(argv[1], size, expected);
  fputs("Unit\n", stdout);
  return 0;
}

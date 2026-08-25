#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <limits>
#include <string>
#include <string_view>
#include <vector>

namespace {

constexpr std::int64_t modulus = 2'147'483'647;
constexpr std::int64_t multiplier = 48'271;

struct Counter {
  std::int64_t total = 0;
  std::int64_t calls = 0;

  void add(std::int64_t value) {
    total += value;
    calls += 1;
  }
};

[[noreturn]] void fail(std::string_view message) {
  std::cerr << message << '\n';
  std::exit(2);
}

std::int64_t modular_product(std::int64_t left, std::int64_t right) {
  const auto product = left * right;
  return product - (product / modulus) * modulus;
}

std::int64_t int_lcg(std::int64_t size) {
  std::int64_t state = 1;
  for (std::int64_t index = 0; index < size; ++index) {
    state = modular_product(state, multiplier);
  }
  return state;
}

std::int64_t periodic_value(std::int64_t index) {
  return index - (index / 1024) * 1024;
}

Counter record_method(std::int64_t size) {
  Counter value;
  for (std::int64_t index = 0; index < size; ++index) {
    value.add(periodic_value(index));
  }
  return value;
}

std::int64_t list_build_scan(std::int64_t size) {
  std::vector<std::int64_t> values;
  for (std::int64_t index = 0; index < size; ++index) {
    values.push_back(periodic_value(index));
  }
  std::int64_t checksum = 0;
  for (std::int64_t index = 0; index < size; ++index) {
    const auto converted = static_cast<std::size_t>(index);
    if (converted >= values.size()) {
      fail("list index outside bounds");
    }
    checksum += values[converted];
  }
  if (values.size() != static_cast<std::size_t>(size)) {
    fail("list length mismatch");
  }
  return checksum;
}

std::int64_t fibonacci(std::int64_t value) {
  if (value < 2) {
    return value;
  }
  return fibonacci(value - 1) + fibonacci(value - 2);
}

std::int64_t parse_number(const char *text, std::string_view message) {
  std::size_t consumed = 0;
  try {
    const auto parsed = std::stoll(text, &consumed, 10);
    if (text[consumed] != '\0' || parsed < 0) {
      fail(message);
    }
    return parsed;
  } catch (...) {
    fail(message);
  }
}

void run(std::string_view name, std::int64_t size, std::int64_t expected) {
  if (size < 0) {
    fail("size must be non-negative");
  }
  if (name == "int_lcg") {
    if (size > 100'000'000) {
      fail("int_lcg size exceeds limit");
    }
    if (int_lcg(size) != expected) {
      fail("int_lcg checksum mismatch");
    }
  } else if (name == "record_method") {
    if (size > 100'000'000) {
      fail("record_method size exceeds limit");
    }
    const auto value = record_method(size);
    if (value.total != expected || value.calls != size) {
      fail("record_method checksum mismatch");
    }
  } else if (name == "list_build_scan") {
    if (size > 10'000'000) {
      fail("list_build_scan size exceeds limit");
    }
    if (list_build_scan(size) != expected) {
      fail("list_build_scan checksum mismatch");
    }
  } else if (name == "fib_recursive") {
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

} // namespace

int main(int argc, char **argv) {
  if (argc != 4) {
    fail("usage: benchmark CASE SIZE EXPECTED");
  }
  const auto size = parse_number(argv[2], "invalid benchmark size");
  const auto expected = parse_number(argv[3], "invalid expected checksum");
  run(argv[1], size, expected);
  std::cout << "Unit\n";
  return 0;
}

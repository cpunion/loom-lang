package main

import (
	"fmt"
	"os"
	"strconv"
)

const (
	modulus    int64 = 2147483647
	multiplier int64 = 48271
)

type counter struct {
	total int64
	calls int64
}

func (value *counter) add(amount int64) {
	value.total += amount
	value.calls++
}

func fail(message string) {
	fmt.Fprintln(os.Stderr, message)
	os.Exit(2)
}

func modularProduct(left, right int64) int64 {
	product := left * right
	return product - (product/modulus)*modulus
}

func intLCG(size int64) int64 {
	state := int64(1)
	for index := int64(0); index < size; index++ {
		state = modularProduct(state, multiplier)
	}
	return state
}

func periodicValue(index int64) int64 {
	return index - (index/1024)*1024
}

func recordMethod(size int64) counter {
	value := counter{}
	for index := int64(0); index < size; index++ {
		value.add(periodicValue(index))
	}
	return value
}

func listBuildScan(size int64) int64 {
	values := make([]int64, 0)
	for index := int64(0); index < size; index++ {
		values = append(values, periodicValue(index))
	}
	checksum := int64(0)
	for index := int64(0); index < size; index++ {
		if index < 0 || index >= int64(len(values)) {
			fail("list index outside bounds")
		}
		checksum += values[index]
	}
	if int64(len(values)) != size {
		fail("list length mismatch")
	}
	return checksum
}

func fibonacci(value int64) int64 {
	if value < 2 {
		return value
	}
	return fibonacci(value-1) + fibonacci(value-2)
}

func run(name string, size, expected int64) {
	if size < 0 {
		fail("size must be non-negative")
	}
	switch name {
	case "int_lcg":
		if size > 100000000 {
			fail("int_lcg size exceeds limit")
		}
		if intLCG(size) != expected {
			fail("int_lcg checksum mismatch")
		}
	case "record_method":
		if size > 100000000 {
			fail("record_method size exceeds limit")
		}
		value := recordMethod(size)
		if value.total != expected || value.calls != size {
			fail("record_method checksum mismatch")
		}
	case "list_build_scan":
		if size > 10000000 {
			fail("list_build_scan size exceeds limit")
		}
		if listBuildScan(size) != expected {
			fail("list_build_scan checksum mismatch")
		}
	case "fib_recursive":
		if size > 45 {
			fail("fib_recursive size exceeds limit")
		}
		if fibonacci(size) != expected {
			fail("fib_recursive checksum mismatch")
		}
	default:
		fail("unknown benchmark case")
	}
}

func main() {
	if len(os.Args) != 4 {
		fail("usage: benchmark CASE SIZE EXPECTED")
	}
	size, error := strconv.ParseInt(os.Args[2], 10, 64)
	if error != nil {
		fail("invalid benchmark size")
	}
	expected, error := strconv.ParseInt(os.Args[3], 10, 64)
	if error != nil {
		fail("invalid expected checksum")
	}
	run(os.Args[1], size, expected)
	fmt.Print("Unit\n")
}

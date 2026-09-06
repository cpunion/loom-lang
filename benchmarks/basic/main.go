package main

import (
	"fmt"
	"os"
	"strconv"
)

const modulus int64 = 1000003

func advance(value int64) int64   { return (value*17 + 23) % modulus }
func alternate(value int64) int64 { return (value*7 + 11) % modulus }

func intLCG(n, seed int64) int64 {
	state := seed % modulus
	for i := int64(0); i < n; i++ {
		state = advance(state)
	}
	return state
}

func fibRecursive(n int64) int64 {
	if n < 2 {
		return n
	}
	return fibRecursive(n-1) + fibRecursive(n-2)
}

type Point struct{ x, y, z int64 }

func step(old Point, delta int64) Point {
	return Point{
		x: (old.y + delta) % modulus,
		y: (old.z + old.x) % modulus,
		z: (old.x + old.y + old.z) % modulus,
	}
}

func recordValue(n, seed int64) int64 {
	p := Point{seed % 97, seed % 193, seed % 389}
	for i := int64(0); i < n; i++ {
		p = step(p, i%31)
	}
	return p.x + p.y + p.z
}

func listBuildScan(n, seed int64) int64 {
	values := make([]int64, 0)
	state := seed % modulus
	for i := int64(0); i < n; i++ {
		state = advance(state)
		values = append(values, state)
	}
	total := int64(0)
	for _, value := range values {
		total += value
	}
	return total
}

func functionValue(n, seed int64) int64 {
	action := advance
	if seed%2 != 0 {
		action = alternate
	}
	state := seed % modulus
	for i := int64(0); i < n; i++ {
		state = action(state)
	}
	return state
}

func integer(text string) int64 {
	value, err := strconv.ParseInt(text, 10, 64)
	if err != nil {
		panic(err)
	}
	return value
}

func main() {
	if len(os.Args) != 4 {
		panic("usage: benchmark CASE N SEED")
	}
	n, seed := integer(os.Args[2]), integer(os.Args[3])
	var checksum int64
	switch os.Args[1] {
	case "int_lcg":
		checksum = intLCG(n, seed)
	case "fib_recursive":
		checksum = fibRecursive(n)
	case "record_value":
		checksum = recordValue(n, seed)
	case "list_build_scan":
		checksum = listBuildScan(n, seed)
	case "function_value":
		checksum = functionValue(n, seed)
	default:
		panic("unknown benchmark case")
	}
	fmt.Println(checksum)
}

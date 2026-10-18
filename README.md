# neurastheniacc

A groundbreaking C -> WASM compiler wielding the unbridled power of LLMs!!

## What is this?

`neurastheniacc` compiles a restricted subset of C down into WASM. It follows the usual two-pass approach: first from C to readable WAT, then from WAT to WASM bytecode. However, there's a trailblazing advance that distinguishes `neurastheniacc` from all previous (lesser? (∩￢з￢)) WASM compilers: **the C->WAT pass is handled purely by an LLM.**

### Benefits

With `neurastheniacc`, you can finally alchemize money directly into performance, without any need for pesky intermediaries like "compiler expertise"! If you're unsatisfied with the defaults' results, try any of the following:

- Use a larger Qwen, merely costing more GPU compute!
- Extend the token limits, reasoning budget, or allowed retries, merely costing more execution time!
- Build your own stochastically optimizing compiler: run `neurastheniacc` in a loop 100 times, and pick the best output!
- Or for the real sickoes, finetune your own C->WAT model and provide a custom GGUF! (Please wash your hands afterward.)

## Try it (or else)!

```sh
# plz builb (๑•́ ω •̀๑)
cargo build --release --locked --bin neurastheniacc

# First use downloads ~2.75 GB of Qwen weights, then compiles.
./target/release/neurastheniacc compile examples/mix.c -o mix.wasm

# Invoke the exported function in a standard Wasm runtime.
wasmtime run --invoke mix mix.wasm 3 5
```

## Under the hood

`neurastheniacc` is fully local. On first run, it downloads a Qwen GGUF and caches it. Inference is then provided during compilation by [llama-cpp-2](https://crates.io/crates/llama-cpp-2). No API keys or calls to inference providers required.

During compilation, `neurastheniacc` validates that your C code is supported, provides your code to Qwen, and asks it nicely to translate it into valid WAT. We then take Qwen's lovingly tensor-crafted WAT (just like wire mother used to make!) and attempt to parse in with [wat](https://crates.io/crates/wat) and build it with [wasmi](https://crates.io/crates/wasmi). What happens if, Lord forbid, the model should generate invalid WAT? Never fear: a simple harness provides the current state and parser feedback back to Qwen for another attempt, with up to 3 turns allowed by default.

## C subset

`neurastheniacc` supports this astonishingly flexible program structure: One C function, returning `uint32_t`, with exactly two `uint32_t` parameters. An optional `#include <stdint.h>` is graciously accepted for your convenience.

### Supported features

This luxurious subset of C features is provided, that you might write the programs of your dreams:

- Initialized `uint32_t` locals, assignments, nested blocks and variable shadowing.
- `+`, `-`, `*`, `&`, `|`, `^`, unary `~`, unary `+` and unary `-`.
- Line and block comments.
- `<<` and `>>`, with a literal shift value from `0u` to `31u`.
- Braced `if` / `else`. All paths must return. If conditions may include:
  - Unsigned comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`)
  - Integer truthiness
- Decimal and hexadecimal 32-bit integer literals with an explicit `u` or `U` suffix.
- Returns, including early returns.

### Unsupported features

These extraneous C features _aren't_ supported, and will surely be missed by nobody:

- Pointers, allocation, and casting
- Arrays
- Loops, recursion, and `else if`
- Function calls
- Frivolous mathemetics such as division, remainder, increment/decrement, or negative numbers
- Globals
- Ternaries, or logical operators in general really
- Compound assignment, comparison expressions used as values
- Macros
- Probably a lot of other stuff
- Don't even _think_ about trigraphs, IOCCC pervert.

### Other restrictions

Your program must be no larger than 32 KiB and 4096 Qwen tokens. You may not use more than 48 levels of nesting.

## CLI options

```sh
# Select a model: (default: qwen-low)
# - `qwen-low` (Qwen3.5-4B-Q4_K_M, 4B, ~2.75GB)
# - `qwen-high` (Qwen3.8-27B-UD-Q4_K_M.gguf, 27B, ~16.5GB)
# - or whatever local GGUF your little heart desires
./target/release/neurastheniacc compile examples/mix.c --model qwen-low
./target/release/neurastheniacc compile examples/mix.c --model qwen-high
./target/release/neurastheniacc compile examples/mix.c --model /path/to/model.gguf

# Enable thinking. Only supported with bundled Qwens. (default: disabled)
./target/release/neurastheniacc compile examples/mix.c \
  --qwen-thinking=true

# Increase the context or per-attempt token budget.
./target/release/neurastheniacc compile examples/mix.c \
  --context 16384 --max-tokens 4096

# Set limit for compilation attempts. (default: 3)
./target/release/neurastheniacc compile examples/mix.c --attempts 5 --verbose

# Print diagnostics.
./target/release/neurastheniacc compile examples/mix.c --verbose

# Run on CPU with however many threads.
./target/release/neurastheniacc compile examples/mix.c --gpu-layers 0 --threads 8

# Download a Qwen to a custom cache, then compile offline.
./target/release/neurastheniacc fetch --model qwen-low --cache-dir .models
./target/release/neurastheniacc compile examples/mix.c \
  --model qwen-low --cache-dir .models --offline
```

Run `./target/release/neurastheniacc compile --help` for all options and defaults.

## Benchmarks

The `neurastheniacc-bench` executable runs a benchmark on the compiler. Eight standard fixtures are provided, alongside as many randomly generated fixtures as your body can handle. Each fixture is run through `neurastheniacc` with all logs and outputs preserved. For all fixtures that successfully compile to WASM, the resulting executable is proptested against a Clang build of the original C. If we get the same outputs from both `neurastheniacc` and Clang across a wide set of random inputs, then we consider it a successful compilation! °˖✧◝(⁰▿⁰)◜✧˖°

```sh
# bliud
cargo build --release --locked --bin neurastheniacc-bench

# Run a benchmark with 32 fixtures, 24 of 'em generated
./target/release/neurastheniacc-bench bench --model qwen-low \
  --generated 24 --seed 20261014 --out-dir benchmarks/my-benchmark
```

### Results

| Model | Thinking | Pass rate | WAT syntax errors | Wasm validation errors | Semantic mismatches | Token limits | Runtime |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Qwen3.5-4B | Off | 71.9% (23/32) | 6 | 1 | 2 | 0 | 2m 13s |
| Qwen3.5-4B | On | 21.9% (7/32) | 0 | 0 | 0 | 25 | 3h 41m 48s |
| Qwen3.8-27B | Off | 96.9% (31/32) | 0 | 0 | 1 | 0 | 4m 39s |
| Qwen3.8-27B | On | 100% (32/32) | 0 | 0 | 0 | 0 | 43m 11s |

Each run used Metal on Apple Silicon, with greedy decoding and five retries. Thinking-off runs allowed 8,192 tokens context and 1,536 tokens output budget. Thinking-on runs allowed 16,384 tokens context and 8,192 tokens output budget.

## FAQs

### Is this some kind of joke?

'tis the best kind of joke– an executable joke.

### What is wrong with you?

Would love to know, please do write my doctor a memo if ya figure it out.

### Why doesn't the WAT->WASM part use an LLM too?

You better believe I tried. Didn't work at all well with small local models.

## Developing

The Nix shell provides the full toolchain needed to develop the project: Rust, Cargo, rustfmt, Clippy, rust-analyzer, Clang/libclang, CMake, and Wasmtime. A healthy state of delirium provides your motivation for attempting.

`neurastheniacc` so far has only been tested on macOS with Apple Silicon. It... might work on Linux? Probably not very fast, though. On Mac inference uses Metal, on Linux it uses your poor overheating CPU. **You** (yes, you!) could probably improve this by contribbing CUDA or something, if you enjoy turning the wheel of samsara.

Enter the pinned development environment manually, run checks, and build:

```sh
# duild
nix develop

# Run checks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Build the compiler
cargo build --workspace --release --locked --bins
```

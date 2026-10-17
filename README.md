# neurastheniacc

An experimental compiler whose code generator is a local language model.

```
restricted unsigned C → Qwen running inside Rust → WAT → .wasm
```

No Ollama, model server, API key, or hosted inference. The executable downloads a pinned GGUF on first use, verifies its SHA-256, and caches it. The generated WebAssembly runs without a model. **This is a reliability experiment, not a production compiler.**

The first implementation uses Rust, statically linked llama.cpp through `llama-cpp-2`, the `wat` assembler, and the `wasmi` interpreter. The front end only validates and normalizes the input. It does not generate instructions. Every instruction in the output comes from the model; the assembler encodes it. Clang is used only as an independent testing oracle.

## Try it

Build requirements: Rust with Cargo, CMake, and a C/C++ toolchain with libclang available for bindgen. On macOS, install Xcode Command Line Tools and CMake. On Debian/Ubuntu, the corresponding packages include `build-essential cmake clang libclang-dev`. Tested here with Rust 1.95 on macOS ARM64. Metal is compiled in on macOS; other platforms use CPU inference by default.

Install [Wasmtime](https://docs.wasmtime.dev/cli-install.html) to run the generated module.

```sh
cargo build --release --locked --bin neurastheniacc

# First use downloads about 16.46 GB, then compiles the example.
./target/release/neurastheniacc compile examples/mix.c -o runs/demo/mix.wasm

# Invoke the exported function in a standard Wasm runtime.
wasmtime run --invoke mix runs/demo/mix.wasm 3 5
# 107
```

The module exports `mix(i32, i32) -> i32`, so `--invoke mix` selects the function to call.

Alternatively, run it with Node.js:

```sh
node --input-type=module - <<'JS'
import { readFileSync } from "node:fs";
const { instance } = await WebAssembly.instantiate(
  readFileSync("runs/demo/mix.wasm")
);
console.log(instance.exports.mix(3, 5) >>> 0);
JS
```

`>>> 0` displays the return value as an unsigned 32-bit integer.

`compile` generates WAT, assembles it, and validates the module and exported signature. If validation fails, it sends the original C, the previous response, and the diagnostic back to the model. It stops at the first valid module or after **five total attempts** (the initial translation plus up to four repairs). Use `--attempts 1` for one-shot compilation, or choose any limit from 1 to 5.

```sh
./target/release/neurastheniacc compile examples/mix.c --attempts 5 -o runs/repair/mix.wasm
```

This loop belongs to the compiler. It uses syntax, Wasm type/structure validation, and export checks; it does not use Clang results or semantic counterexamples. A valid but incorrect translation stops the loop too. Each retry has a fresh inference context with the same system prompt, source, token budget, and decoding settings; it receives only the most recent candidate and diagnostic, avoiding unbounded history growth. Token-limit failures can be retried; inference/context errors stop immediately.

All attempts are retained in a unique directory beside the output (`mix.attempts-…/`), with exact inputs, responses, extracted WAT, diagnostics, and validated binaries. The final report lists that directory and every attempt. The requested `.wasm` is written only after success. Unsupported source is rejected before inference; the only text cleanup is removal of an optional single Markdown code fence.

## Model and cache

`--model high` is the default. Both presets download pinned weights on first use and verify their size and SHA-256 before caching them.

| Selection | Model | Download | Chat mode |
| --- | --- | ---: | --- |
| `high` (default) | [Qwen3.8-27B UD-Q4_K_M](https://huggingface.co/unsloth/Qwen3.8-27B-GGUF) | 16.46 GB | Non-thinking, configured automatically |
| `low` | [Qwen3.5-4B Q4_K_M](https://huggingface.co/unsloth/Qwen3.5-4B-GGUF) | 2.74 GB | Non-thinking, configured automatically |

The pinned revisions and checksums live in `src/model.rs`. The cache is `$XDG_CACHE_HOME/neurastheniacc` if set, otherwise `~/Library/Caches/neurastheniacc` on macOS or `~/.cache/neurastheniacc` elsewhere. Override it with `--cache-dir`. Downloads use a temporary file and are atomically installed only after size and checksum validation. Interrupted downloads are discarded; the next attempt starts again. Cached weights are verified on each model-using invocation.

```sh
./target/release/neurastheniacc fetch                  # Cache high ahead of time
./target/release/neurastheniacc --model low fetch      # Cache low ahead of time
./target/release/neurastheniacc compile examples/mix.c # Uses high
./target/release/neurastheniacc --model low compile examples/mix.c
./target/release/neurastheniacc --offline compile examples/mix.c
./target/release/neurastheniacc --model low --gpu-layers 0 compile examples/mix.c
./target/release/neurastheniacc --model /path/to/another.gguf compile examples/mix.c
```

`--offline` fails if the selected preset is absent, without contacting the network. `--model` also accepts a local GGUF path and records its hash; it requires a chat template supported by llama.cpp. GPU acceleration requires access to the GPU, which some application sandboxes restrict. `--gpu-layers 0` explicitly selects CPU inference. The first Metal inference may also incur kernel initialization costs.

Both presets automatically use Qwen's text-only chat template with `enable_thinking=false`, including its empty thinking prefix. Override this with `--qwen-thinking` to use the model's thinking format (including the cached high model's default `xhigh` effort instruction). For example, use `--qwen-thinking --context 16384 --max-tokens 8192`; reasoning and the final answer share that output budget. Thinking mode preserves the raw response and saves separate `reasoning.txt` and `answer.txt` files in each attempt directory. Only text after the closing `</think>` is passed to the WAT parser; a missing closing delimiter is rejected. The non-thinking default is unchanged. For a custom Qwen GGUF path, request this with `--qwen-no-thinking`. The setting is recorded in compilation reports. The compiler system prompt stays unchanged, and every model response is preserved. Repairs are new model responses driven by validator feedback, not edits made by the compiler.

The weights are the only downloaded runtime asset; there is no background process to manage. Compilation and Wasm execution do not require Clang.

## Supported C subset

One function returning `uint32_t`, with exactly two `uint32_t` parameters. An optional `#include <stdint.h>` is accepted. The validation target has 32-bit `unsigned int`.

Supported:

- Initialized `uint32_t` locals, assignments, nested blocks and variable shadowing.
- `+`, `-`, `*`, `&`, `|`, `^`, unary `~`, unary `+` and unary `-`.
- `<<` and `>>` with a literal count from `0u` to `31u`.
- Unsigned comparisons `==`, `!=`, `<`, `<=`, `>`, `>=` in `if` conditions; integer truthiness is also accepted.
- Braced `if` / `else`, and early `return`. All paths must return.
- Decimal and hexadecimal 32-bit integer literals with an explicit `u` or `U` suffix.
- Line and block comments.

No pointers, allocation, arrays, loops, calls, recursion, casts, division, remainder, globals, signed types, increment/decrement, compound assignments, logical operators, ternaries, macros, or comparison expressions used as values. `else if` must be written as `else { if (...) { ... } }`. Shift counts cannot be computed expressions; write `(x << 2u) + y`, not `x << 2u + y`. Self-referential local initializers are rejected even when they could refer to an outer name. Identifiers must begin with an ASCII letter and cannot be reserved words or the small oracle harness's reserved names.

Source is capped at 32 KiB, 4096 tokens and 48 nesting levels. C line splicing and trigraphs are rejected. The validated token stream, with comments and the include removed, is sent to both the model and Clang. The oracle compiles the function in its own translation unit under a private symbol to avoid collisions with harness names and I/O macros.

## Benchmarking

The separate `neurastheniacc-bench` executable provides `run` for execution, `check` for comparing an artifact against Clang, and `bench` for compiling and evaluating a corpus. `check` and `bench` require Clang. Test failures do not change compilation reports or remove artifacts.

```sh
cargo build --release --locked --bin neurastheniacc-bench

# Run a saved artifact in the harness.
./target/release/neurastheniacc-bench run runs/demo/mix.wasm --export mix 3 5

# Compare it against Clang with a different input seed.
./target/release/neurastheniacc-bench check examples/mix.c runs/demo/mix.wasm --cases 10000 --seed 42

# Eight fixed programs plus 24 generated programs; output directory must be new.
./target/release/neurastheniacc-bench bench --model low --offline \
  --generated 24 --seed 20261014 --out-dir runs/my-baseline

# Compare prompts while preserving the original result.
./target/release/neurastheniacc-bench bench --model low --attempts 1 --offline --prompt prompts/v1.txt \
  --generated 8 --out-dir runs/original-prompt

# Reproduce the one-shot v4 corpus with Qwen3.8-27B.
./target/release/neurastheniacc-bench bench \
  --model high --attempts 1 \
  --generated 24 --seed 20261014 --out-dir runs/qwen38-v4

# Enable thinking for C → WAT, with up to five attempts.
./target/release/neurastheniacc-bench bench --model high --offline \
  --qwen-thinking --context 32768 --max-tokens 8192 \
  --prompt prompts/v4.txt \
  --attempts 5 --generated 24 --cases 1000 --seed 20261014 \
  --out-dir runs/c-to-wat-thinking
```

Measured results are in [benchmarks/README.md](benchmarks/README.md), with full raw responses in the archived JSON reports. In the historical one-shot runs on the same frozen 32-program corpus, the original 1.5B baseline passed 4/32, `low` (Qwen3.5-4B) passed 17/32, and `high` (Qwen3.8-27B) passed 31/32. Median compilation times were 0.73, 1.96, and 10.05 seconds respectively.

Each function is tested against Clang `-std=c11 -O2` on a fixed 18 × 18 grid of edge values plus 1,000 seeded random input pairs by default. `--cases` changes the random count; the 324 edge pairs are always included. Testing stops at the first mismatch or runtime error and reports both the planned and executed counts. The RNG algorithm is fixed for reproducibility.

The benchmark calls the same compiler loop as `compile`, with five total attempts by default. `bench --attempts 1` reproduces the one-shot baselines. The model stays loaded across a benchmark, but every generation attempt receives a fresh inference context. Decoding is greedy. This does **not** promise identical bytes across hardware or inference-library versions.

Every candidate is checked for a correct `(i32, i32) -> i32` export. Execution forbids imports, start functions, globals, memories and tables. Each input runs in a fresh Wasm instance with a 100,000-unit fuel limit. These restrictions are part of this prototype's contract.

Outputs include:

- `.raw.txt`: unmodified final model response; earlier responses live in the attempt directory.
- `.wat`: extracted candidate, even if assembly fails.
- `.wasm`: valid candidate, including candidates that fail differential checking.
- `.report.json`: compilation result (schema 7), every attempt and feedback input, source/model/output hashes, inference settings, token counts, stop reasons and total timing.
- `.check.json`: benchmark evaluation result and counterexample, with the compilation report nested separately.
- `summary.json`: all benchmark reports, aggregate outcomes and Clang version.

Compilation reports store generations once, in `attempts[].generation`; the final response is in the last attempt. Settings record one `chat_mode`: `model_template`, `qwen_no_thinking`, or `qwen_thinking`.

A benchmark exits nonzero if any program fails, **after** saving its results. Compilation reports distinguish generation errors, truncation, malformed output, invalid Wasm and successful compilation. Harness reports separately record check errors, runtime errors, mismatches and passed tests. Source and model-loading errors stop before the benchmark can run.

Compilation time includes inference context creation, prompt processing, generation, assembly and module validation. Weight loading is reported separately; downloading and hashing are outside the compilation measurement. Wasm execution time includes fresh instance creation. The Clang reference timing includes compiling and running the oracle. These are not apples-to-apples code-performance benchmarks.

The eight fixed fixtures exercise addition, unsigned comparison, overflow, shifts, mutation, nested branches, shadowing and complement. Generated programs use four templates with seeded constants and operators. This is a small, structured smoke corpus, not a representative sample of C and not proof of equivalence. SMT verification is deliberately deferred.

The compiler uses its bundled system prompt and writes no prompt copies or prompt hashes. The benchmark alone supports `bench --prompt` for prompt-tuning experiments and saves one `system.prompt.txt` per corpus run.

The original demo was used to develop prompts v1–v4. Keep these versions, and keep later prompt tuning separate from evaluation sets. DSPy could optimize the system prompt later through `bench --prompt`; it is not a runtime dependency.

## Development

```sh
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
cargo fmt --check
```

The tests require Clang but no model or network. They check front-end rejection, wrapping arithmetic, unsigned comparison counterexamples, bounded Wasm execution, forbidden module features, generated source validity, cache failure modes, strict output extraction, feedback propagation, retry limits, and stopping on validation success without semantic feedback.

- `src/source.rs`: rejecting C front end; no code generation.
- `src/model.rs`: cache, checksums and embedded inference.
- `src/prompt.rs`: Qwen chat formatting and reasoning boundaries.
- `src/compile.rs`: compilation pipeline, artifacts and compilation reports.
- `src/wasm.rs`: Wasm validation and execution.
- `src/eval.rs`: test harness, native Clang oracle, inputs and corpus generator.
- `src/main.rs`: compiler CLI (`fetch`, `compile`).
- `src/bin/neurastheniacc-bench.rs`: execution and evaluation CLI (`run`, `check`, `bench`).
- `src/cli.rs`: shared model options; `src/lib.rs`: shared library entry point.
- `prompts/`: versioned prompts.
- `fixtures/eval/`: fixed smoke corpus.

`Cargo.lock` pins Rust dependencies, including the bundled llama.cpp revision supplied by `llama-cpp-2`. No SMT solver, DSPy service, or remote inference is involved.

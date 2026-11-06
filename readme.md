# LSTM

Interpreter and renderer for parametric, context-aware, open stochastic L-systems with recursive interpretation rules.

Build with a Rust toolchain supporting edition 2024 and Cargo:

```sh
cargo build --release
./target/release/lstm lstms/lindenmayer_algae.lstm
```

Usage: `lstm [OPTIONS] <FILE>`, where `<FILE>` is a JSON5 L-system model (examples are in `lstms/`). You can also run it with `cargo run --release -- <FILE> [OPTIONS]`.

By default, the program opens a window displaying the final generation. Close the window or press Escape to exit. Use `--animate` to display successive generations, or `--output` to render without opening a window.

| Argument | Description |
| --- | --- |
| `-n <N>` | Number of rewrites after the axiom (generation 0). Defaults to the model's `recommended_iterations`, or 5. Use `-n -1 --animate` for endless live animation. Exports require a nonnegative count. |
| `-a`, `--animate` | Animate successive generations in the live viewer. |
| `-m`, `--axiom <STRING>` | Override the model's axiom. Quote strings containing spaces or shell metacharacters. |
| `-f`, `--fps <FPS>` | Animation frame rate (default: 2). Must be finite; GIF export requires a positive value. Nonpositive values disable the live animation delay. |
| `-g`, `--globals <DEFINITIONS>` | Override or add global formulas using comma-separated `name: expression` pairs, e.g. `--globals 'scale: 2, cell_size: 8'`. Optional surrounding braces are accepted. |
| `-s`, `--seed <SEED>` | Override the model's random seed with an integer or text seed for reproducible runs. |
| `-o`, `--output <PATH>` | Export a `.png` of the final generation or a looping `.gif` containing generations 0 through N (N + 1 frames). GIF export enables animation automatically. |
| `-W`, `--width <PIXELS>` | Override canvas width (model value, or 1000 by default). |
| `-H`, `--height <PIXELS>` | Override canvas height (model value, or 800 by default). |
| `-h`, `--help` | Show command-line help. |
| `-V`, `--version` | Show the program version. |

Canvas dimensions must each be between 1 and 65,535 pixels, with at most 64,000,000 pixels total. The output directory must already exist.

```sh
# Animate ten rewrites at four frames per second.
./target/release/lstm lstms/lindenmayer_algae.lstm -n 10 --animate --fps 4

# Export a reproducible final image with custom dimensions.
./target/release/lstm lstms/lindenmayer_algae.lstm -n 10 --seed 42 -W 1200 -H 900 -o algae.png

# Export an animation, including the axiom as the first frame.
./target/release/lstm lstms/lindenmayer_algae.lstm -n 10 --fps 4 -o algae.gif
```

Optional tracing helps inspect rule evaluation:

| Argument | Description |
| --- | --- |
| `--trace` | Write trace events to standard error. |
| `--trace-output <PATH>` | Write traces to a file instead; also enables tracing. |
| `--trace-format <FORMAT>` | Select `text` (default) or `jsonl`. Does not enable tracing by itself. |
| `--trace-symbol <SYMBOL>` | Filter by symbol; repeat to include multiple symbols. Uses Unicode NFC normalization, so canonically equivalent spellings match. |
| `--trace-rule <RULE>` | Filter by rule text or index; repeat to include multiple rules. |
| `--trace-generations <RANGE>` | Filter by one generation (`3`) or an inclusive range (`0:3`). |
| `--trace-draw` | Include drawing-phase events. |

Trace filters and `--trace-draw` also enable tracing. For example:

```sh
./target/release/lstm lstms/lindenmayer_algae.lstm -n 3 -o algae.png --trace-output trace.jsonl --trace-format jsonl --trace-generations 0:3
```

The full syntax is described in `syntax_lstm_en.pdf`

[This program is dependant on my `formulas` crate available here](https://github.com/Hayen03/formulas).

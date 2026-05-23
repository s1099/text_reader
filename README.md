# Text Reader

A text reader designed to read huuuge files while having a low memory footprint that runs natively.

Tested with text file consisting of 1 Million lines (~9GB), used close to 20MB of memory with the file open and being able to jump around anywhere instantly. Results may differ due to storage/cpu speeds.

## Running

Make sure to have [rust installed](https://rustup.rs/)

```bash
cargo build --release

# or to build and run
cargo run --release
```
Build will be in `target/release/`

# Prism fuzz targets

Requires a nightly toolchain and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
(Linux or macOS; libFuzzer is not supported on Windows MSVC).

```bash
cargo install cargo-fuzz
# Seed the corpus with the sample files
mkdir -p corpus/parse_any && cp ../test-files/* corpus/parse_any/
cargo +nightly fuzz run parse_any -- -max_total_time=300 -rss_limit_mb=2048 -timeout=20
cargo +nightly fuzz run sanitize_html -- -max_total_time=120
```

Crashes are written to `artifacts/<target>/`. Reproduce with
`cargo +nightly fuzz run parse_any artifacts/parse_any/<file>`.

On Windows, `cargo test -p prism-tests --test robustness_tests` runs a
deterministic mutation test over the sample corpus instead.

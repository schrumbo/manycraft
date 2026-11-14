# Contributing

Thanks for helping. Manycraft is early, so open an issue before larger changes to agree on the approach.

## Setup

1. Install stable [Rust](https://rustup.rs) and JDK 25.
2. On Debian or Ubuntu, install the system libraries listed in the [README](README.md#build-from-source).
3. `cargo run` starts the app, `cargo test` runs the tests, `./gradlew build` in `mod/` builds the companion mod.

## Pull requests

- One change per pull request, with a short description of what it fixes or adds and how you tested it.
- `cargo test` and `cargo clippy` pass, and the mod builds if you touched `mod/`.
- Match the surrounding style. Do not run `cargo fmt` over files you did not change.
- No code comments; doc comments on public items only.
- New behaviour comes with tests. Bugs start with a failing test.
- UI changes include screenshots in light and dark mode, and work with mouse and keyboard.
- Never commit tokens, account names or other personal data, including in logs or test fixtures.

## Scope

Manycraft supports Fabric on Windows, macOS and Linux. Features that would break a server's rules for everyone, such as automation that plays for you, are out of scope. The [Code of Conduct](CODE_OF_CONDUCT.md) applies to every issue and pull request.

## License

By contributing you agree that your work is licensed under the [GNU AGPL v3.0](LICENSE).

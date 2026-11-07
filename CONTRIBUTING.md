# Contributing

Thanks for helping with j3code. Bug reports, fixes, and focused features are welcome.

## Before you start

- Search [existing issues](https://github.com/jtyuill/j3code/issues) first.
- For anything larger than a small fix, open an issue to discuss it before writing code.
- Read [PLAN.md](PLAN.md). It records scope decisions, such as omp as the only agent and no authentication beyond the tailnet. Changes that go against it need a discussion first.

## Setup

```sh
git clone git@github.com:jtyuill/j3code.git
cd j3code
nix develop        # optional: Rust toolchain, CMake, mold, Linux GUI and audio libraries
                   # (with direnv, `direnv allow` loads it on every `cd` instead)
pnpm install       # only needed for the TypeScript packages and iOS app
```

Without Nix you need Rust 1.88+ and CMake, plus on Linux the libraries installed in [`ci.yml`](.github/workflows/ci.yml). You need `omp` installed to run agents.

## Making changes

1. Branch from `main`.
2. Keep each pull request to one logical change.
3. Add or update tests for behavior you change.
4. Run the checks CI runs:

   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets
   cargo test --workspace --locked
   pnpm typecheck && pnpm test    # if you touched TypeScript
   ```

   Tests that spend tokens or need a live `omp` are `#[ignore]`d. Run them only when you change that code.
5. If you change the wire protocol, update `docs/protocol` and the TypeScript contracts in the same pull request. The server and iOS app change together.

## Commit messages

- Short imperative subject, ideally under 72 characters. A prefix such as `feat(j3-desktop):` or `fix(j3-core):` is welcome.
- Use the body to say why, not to restate the diff.

## Pull requests

- Describe what changed and why, and how you tested it.
- Link the issue it closes.
- CI must pass on Linux and macOS.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).

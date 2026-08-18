# CKB Treasury — Agent Instructions

## Toolchains

- **Root**: `impl/rust-toolchain.toml` pins Rust 1.95.0
- **on-chain scripts**: The projects in `impl/contracts` are compiled targeting RISC-V for CKB, using stable Rust 1.95.0. Do not mix this environment or its build artifacts with other Rust projects.

## Documents
The `docs/*.md` files contain specifications.

- When using the CCC library, refer to [ccc](https://github.com/XuJiandong/ckb-vote-poc/blob/main/docs/knowledge/ccc.md).
- When using the `ckb-cli` tool, refer to [ckb-cli](https://github.com/XuJiandong/ckb-vote-poc/blob/main/docs/knowledge/ckb-cli.md).
- When working with the devnet, refer to [devnet](https://github.com/XuJiandong/ckb-vote-poc/blob/main/docs/knowledge/devnet.md).
- When working with CKB RPC, refer to [ckb rpc](https://github.com/XuJiandong/ckb-vote-poc/blob/main/docs/knowledge/rpc.md).

## Implementations
The spec in docs/proposal-type-script.md has an implementation in the ../ckb/script project, primarily under src/proposal. It is an embedded on-chain script implemented in the node.
Build ckb with:
```
RUSTFLAGS="-C debuginfo=0" cargo build --locked --bin ckb --features "with_sentry,with_dns_seeding,portable"
```

The binary `ckb` is located at target/debug/ckb. Copy it back to impl/devnet/ckb.

The spec in docs/vote-type-script.md has an implementation in ./impl/contracts/vote-type-script. It is an on-chain script.

## Development workflow

After making changes in `impl` folder, run the following in order:

### 1. Format & Clippy

```sh
cd impl && make fmt && make clippy
```

### 2. Test

```sh
cd impl && make test
```

Do NOT report the task as done until they succeed.
If you skip this, you failed the task even if the edit is correct.

### SDK
When working with content in the impl/sdk folder, refer to [this document](./impl/sdk/AGENTS.md).

## E2E Tests
The test is located at impl/e2e/run-devnet.sh. See more instructions in impl/e2e/AGENTS.md.

## On-Chain Script (Contract) Implementation
in folder impl/contracts.

These scripts should be implemented in Rust using [ckb-std](https://github.com/nervosnetwork/ckb-std).
When using syscalls, prefer the `high_level` API. If a high-level equivalent is unavailable, fall back to the low-level syscalls.
Review the relevant [RFCs](https://github.com/nervosnetwork/rfcs/tree/master/rfcs) before starting implementation.

### Target and memory

- Contract crates are `no_std` RISC-V programs targeting `riscv64imac-unknown-none-elf`; test and off-chain crates use the native target and `std`.
- CKB-VM provides 4 MiB of memory. Prefer `ckb_std::default_alloc!` and the default linker layout; change heap sizing or linker configuration only with measured justification.
- Keep the pinned root toolchain. If C code is required, use Clang 17 or newer.

### Transaction data and dependencies

- Use packed transaction types exposed by `ckb-std::ckb_types` or `ckb-gen-types` instead of parsing Molecule structures by hand. Transaction layout is defined by [RFC 0022](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0022-transaction-structure/0022-transaction-structure.md).
- Generated Molecule code should be committed and excluded from Clippy linting. Do not introduce a `build.rs` generator unless explicitly requested.
- Contract dependencies must support `no_std`. Disable their default features and enable only required contract features; keep `ckb-gen-types` default features disabled.
- Do not enable `ckb-std`'s `native-simulator` feature unless the task explicitly calls for simulator or coverage work.

### Binary size and tests

- Compare stripped contract sizes before and after dependency or code changes with `cd impl && make size`. Report increases above 100 KiB and warn when any final ELF exceeds 400 KiB; CKB's executable limit is roughly 500 KiB.
- Use `ckb-testtool` and `Context::verify_tx` for contract tests. Add failure cases for changed validation paths and avoid `verify_and_dump_failed_tx` unless transaction dumps are explicitly needed.

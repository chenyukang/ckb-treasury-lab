# Treasury Voting Benchmarks

## Optimistic batch settlement

The implemented benchmark is the ignored test
`contract_tests::tests::benchmark_tally_batch_cycles`. It creates independent
voters and one DAO outpoint per voter. The builder-produced transaction then runs
in CKB-VM through `ckb-testtool`.

Run it with:

```bash
cd impl
make build
cargo test -p contract-tests benchmark_tally_batch_cycles -- --ignored --nocapture
```

Measured on Apple M2 Pro (`arm64`) with Rust 1.95.0 and the contract build's
release profile plus debug assertions.

### Reproduced V1 baseline

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,250,782 | 3,250,782 | 670 B | 1,390 B |
| 10 | 32,333,827 | 3,233,382 | 7,509 B | 8,229 B |
| 50 | 162,938,523 | 3,258,770 | 41,093 B | 41,813 B |
| 100 | 327,064,874 | 3,270,648 | 85,387 B | 86,107 B |

### Optimized V3

V3 preserves the same header commitment, RawTransaction hashing, reducer, old/new
state-root verification, and final omission challenges. It changes only the
proof representation and deterministic implementation:

- transition keys must be strictly increasing, replacing quadratic duplicate
  detection with one linear pass;
- all relevant transactions from one block share one CBMT multiproof and one
  copy of block metadata;
- vote, DAO-outpoint, and processed-event leaves use BLAKE2b domain-separated
  namespaces in one SMT, verified once against both the old and new roots;
- the tally witness has an explicit V3 version, so older encodings are rejected.

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,292,919 | 3,292,919 | 662 B | 1,382 B |
| 10 | 31,740,341 | 3,174,034 | 5,977 B | 6,697 B |
| 20 | 63,291,558 | 3,164,577 | 11,885 B | 12,605 B |
| 21 | 66,409,707 | 3,162,367 | 12,468 B | 13,188 B |
| 22 | 69,548,994 | 3,161,317 | 13,065 B | 13,785 B |
| 23 | 72,695,867 | 3,160,689 | 13,654 B | 14,374 B |
| 50 | 157,696,837 | 3,153,936 | 29,605 B | 30,325 B |
| 100 | 314,427,610 | 3,144,276 | 59,099 B | 59,819 B |
| 200 | 628,181,614 | 3,140,908 | 118,189 B | 118,909 B |
| 500 | 1,566,878,583 | 3,133,757 | 295,281 B | 296,001 B |

At 100 independent votes, V3 reduces cycles by 3.9% and witness bytes by 30.8%
relative to the reproduced V1 baseline. The verification cost remains linear at
about 3.13M to 3.17M cycles per independent vote.

The default 70M `tx_pool.max_tx_verify_cycles` is not a consensus or current
transaction-admission ceiling. It is primarily the local threshold that marks a
remote transaction as large-cycle for verification-worker scheduling. Therefore,
22 votes at 69.55M and 23 votes at 72.70M fall on different sides of that
scheduling threshold, but both remain valid transaction-pool and relay candidates.

Consensus block verification gives each transaction up to the consensus
`max_block_cycles` and also requires the sum for the block to stay within that
same limit. A 100-vote batch uses about 9.0% of CKB's default 3.5B block cycle
limit and 10.0% of the default 597,000-byte block limit, so it fits both measured
consensus resources. Raising `tx_pool.max_tx_verify_cycles` is not required to
accept it; doing so mainly lets more workers handle it as a small-cycle
transaction and reduces the intended isolation of expensive remote transactions.

The implemented 100-event hard cap is therefore a reasonable current builder
target for the independent-vote shape, rather than a 20-event ceiling. These
figures are still preliminary: revote-heavy, multi-DAO-deposit, DAO-spend, sparse
cross-block, and adversarial proof shapes need separate measurements before the
production cap is frozen.

## Legacy node-scan proposal benchmark

Unlike a normal script on CKB, the proposal type script needs to perform calculations over a large number of blocks, which could become a bottleneck. Hence we need to design a benchmark and measure it.

## Probe
We add USDT (User Statically-Defined Tracing) to the proposal type script for this task. We define the following probes:

```rust
#[usdt::provider]
pub mod proposal_probe {
    fn verify_entry() {}
    fn verify_exit() {}
    fn block_provider_entry() {}
    fn block_provider_exit() {}
}
```

They measure the `verify` function and block loading. The former gives an overview, while block loading reveals where the bottleneck lies.

## Steps to Bench
1. Build ckb with the `probe` feature enabled (it is enabled by default).
2. Run `e2e/start.sh` to start ckb.
3. Run `e2e/benchmark.sh` to start the benchmark.
4. Run `e2e/run-devnet.sh` to start the test case. Use `export DURATION=N` to specify the duration to measure.
5. Press `Ctrl+C` on `benchmark.sh` when testing is done.

## Results
The following scenarios were used:
1. 1000 blocks
2. Each block contains 1051 transactions
3. Machine: Intel(R) Xeon(R) Platinum 8275CL CPU @ 3.00GHz, 4 cores.

Results are as follows:
```text
[verify ] TID 222364  call# 1     6114         ns
[verify ] TID 222366  call# 1     2087422275   ns
^C
╔════════════════════════════════════════════════════╗
║                verify()  Summary                   ║
╚════════════════════════════════════════════════════╝
@verify_count: 2
@verify_total_ns: 6382395685
@verify_avg_ns: 3191197842

@verify_min_ns: 6114
@verify_max_ns: 6382389571

  Latency distribution (ns):
@verify_quant_ns:
[4K, 8K)               1 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[8K, 16K)              0 |                                                    |
[16K, 32K)             0 |                                                    |
[32K, 64K)             0 |                                                    |
[64K, 128K)            0 |                                                    |
[128K, 256K)           0 |                                                    |
[256K, 512K)           0 |                                                    |
[512K, 1M)             0 |                                                    |
[1M, 2M)               0 |                                                    |
[2M, 4M)               0 |                                                    |
[4M, 8M)               0 |                                                    |
[8M, 16M)              0 |                                                    |
[16M, 32M)             0 |                                                    |
[32M, 64M)             0 |                                                    |
[64M, 128M)            0 |                                                    |
[128M, 256M)           0 |                                                    |
[256M, 512M)           0 |                                                    |
[512M, 1G)             0 |                                                    |
[1G, 2G)               0 |                                                    |
[2G, 4G)               0 |                                                    |
[4G, 8G)               1 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|


╔════════════════════════════════════════════════════╗
║          BlockProvider calls  Summary              ║
╚════════════════════════════════════════════════════╝
@bp_count: 1002
@bp_total_ns: 5837837932
@bp_avg_ns: 5826185

@bp_min_ns: 2491
@bp_max_ns: 17768858

  Latency distribution (ns):
@bp_quant_ns:
[2K, 4K)               1 |                                                    |
[4K, 8K)               1 |                                                    |
[8K, 16K)              0 |                                                    |
[16K, 32K)             0 |                                                    |
[32K, 64K)             0 |                                                    |
[64K, 128K)            0 |                                                    |
[128K, 256K)           0 |                                                    |
[256K, 512K)           0 |                                                    |
[512K, 1M)             0 |                                                    |
[1M, 2M)               0 |                                                    |
[2M, 4M)             477 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[4M, 8M)             149 |@@@@@@@@@@@@@@@@                                    |
[8M, 16M)            373 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@            |
[16M, 32M)             1 |                                                    |
```

It costs 2.1 seconds in total. Normalized to 1 day, that is 22.7 seconds (158.8 seconds for 7 days).
Although this scenario is at maximum throughput, the processing time is significant. We need a plan to reduce the total workload.

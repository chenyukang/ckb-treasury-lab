# Treasury Voting Benchmarks

## Optimistic batch settlement V1

The implemented benchmark is the ignored test
`contract_tests::tests::benchmark_tally_batch_cycles`. It creates independent
voters, one DAO outpoint per voter, CBMT inclusion proofs for every VoteTx, and
old/new proofs for all three SMTs. The builder-produced transaction then runs in
CKB-VM through `ckb-testtool`.

Run it with:

```bash
cd impl
make build
cargo test -p contract-tests benchmark_tally_batch_cycles -- --ignored --nocapture
```

Measured on Apple M2 Pro (`arm64`) with Rust 1.95.0 and the contract build's
release profile plus debug assertions:

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,253,229 | 3,253,229 | 670 B | 1,390 B |
| 10 | 32,328,142 | 3,232,814 | 7,505 B | 8,225 B |
| 50 | 163,068,826 | 3,261,376 | 41,111 B | 41,831 B |
| 100 | 326,974,360 | 3,269,743 | 85,387 B | 86,107 B |

The cost is approximately linear at 3.25M to 3.27M cycles per independent vote.
At a 50M to 60M operational target, a batch should contain roughly 15 to 18
independent votes. A 100-vote batch uses about 9.3% of CKB's default 3.5B block
cycle limit and about 14.4% of the default 597,000-byte block limit. Cycles reach
the chosen operational target before transaction bytes do.

These figures are preliminary. Revote-heavy, multi-DAO-deposit, DAO-spend, and
adversarial proof shapes still need separate measurements before production
parameters are frozen.

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

